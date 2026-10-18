"""Scans the crates cargo vet exempts for capabilities and suspicious content.

Usage: python3 -I ci/capability-scan.py [findings.jsonl]   (after `cargo fetch --locked`)

Reads the sources Cargo downloaded and runs nothing from them. Looks for process spawning,
network and dynamic loading, references to credentials (SSH keys, cloud and package
registry credentials, wallets, keychains), long encoded blobs, executables shipped in the
crate, and, in build scripts and procedural macros, which run while Freya is built: file
writes, environment variables beyond Cargo's own, and addresses. Prints a count per kind and
writes the findings, one JSON object a line, for review (see docs/DEPENDENCIES.md).
"""
import glob
import json
import os
import re
import sys

CARGO_HOME = os.environ.get("CARGO_HOME", os.path.expanduser("~/.cargo"))
REGISTRIES = glob.glob(os.path.join(CARGO_HOME, "registry", "src", "index.crates.io-*"))
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

PATTERNS = {
    "process": re.compile(r"\b(?:std::)?process::Command\b|\bCommand::new\s*\(|\bexecv?p?\s*\(|\bposix_spawn|\bCreateProcess[AW]?\b|\bShellExecute"),
    "network": re.compile(r"\bTcpStream\b|\bUdpSocket\b|\bTcpListener\b|\breqwest::|\bureq::|\bhyper::Client|\bcurl::|\bsocket\s*\(\s*AF_INET|\bconnect\s*\(.*sockaddr|\bWinHttp|\bInternetOpen"),
    "url": re.compile(r"https?://[^\s\"')]+"),
    "secret-path": re.compile(r"\.ssh\b|id_rsa|id_ed25519|\.aws/|\.npmrc|\.pypirc|cargo/credentials|\.gnupg|wallet\.dat|Keychains/|\.git-credentials|kube/config|BEGIN (?:RSA |OPENSSH )?PRIVATE KEY", re.I),
    "secret-env": re.compile(r"env::var(?:_os)?\(\s*\"(?:[^\"]*(?:TOKEN|SECRET|PASSWORD|API_KEY|AWS_|GITHUB|NPM|SSH)[^\"]*)\"", re.I),
    "base64-blob": re.compile(r"[A-Za-z0-9+/]{400,}={0,2}"),
    "hex-blob": re.compile(r"(?:0x[0-9a-fA-F]{2}\s*,\s*){400,}"),
    "dynamic-load": re.compile(r"\bdlopen\b|\bLoadLibrary[AW]?\b|libloading::"),
}
BUILD_FS_WRITE = re.compile(r"fs::(?:write|create_dir_all|copy|rename|remove_file|remove_dir_all|File::create)|File::create")
ENV_READ = re.compile(r"env::var(?:_os)?\(\s*\"([^\"]+)\"")
HARMLESS_ENV = re.compile(r"^(CARGO|OUT_DIR|TARGET|HOST|PROFILE|OPT_LEVEL|DEBUG|NUM_JOBS|RUSTC|RUSTDOC|DOCS_RS|PKG_CONFIG|CC|CXX|AR|CFLAGS|CXXFLAGS|LDFLAGS|MACOSX_DEPLOYMENT_TARGET|IPHONEOS_DEPLOYMENT_TARGET|SDKROOT|DEP_|PATH$|VCPKG|LIB$|INCLUDE$|WINDIR|ANDROID|NDK|EMSCRIPTEN|WASI)")
# Executables and libraries (ELF, Mach-O, fat Mach-O, PE, static archives), and COFF objects by
# their machine (x86-64, x86, ARM64, ARM), which have no magic of their own.
EXEC_MAGIC = [b"\x7fELF", b"\xcf\xfa\xed\xfe", b"\xce\xfa\xed\xfe", b"\xca\xfe\xba\xbe", b"MZ", b"!<arch>"]
COFF_MACHINES = [b"\x64\x86", b"\x4c\x01", b"\x64\xaa", b"\xc4\x01"]
# Prebuilt code by its name, whatever it starts with.
BINARY_EXT = {".o", ".obj", ".so", ".dylib", ".dll", ".exe"}
TEXT_EXT = {".rs", ".c", ".h", ".cc", ".cpp", ".hpp", ".s", ".S", ".asm", ".py", ".sh", ".pl", ".toml", ".js", ".json", ".txt", ".md", ".in", ".cmake", ".bat", ".ps1"}


def build_time_files(root):
    """build.rs and, for proc-macro crates, all .rs files."""
    files = set()
    cargo = os.path.join(root, "Cargo.toml")
    text = open(cargo, errors="replace").read() if os.path.exists(cargo) else ""
    m = re.search(r'^build\s*=\s*"([^"]+)"', text, re.M)
    if m:
        files.add(os.path.normpath(os.path.join(root, m.group(1))))
    elif os.path.exists(os.path.join(root, "build.rs")) and not re.search(r"^build\s*=\s*false", text, re.M):
        files.add(os.path.join(root, "build.rs"))
    proc_macro = bool(re.search(r"^proc-macro\s*=\s*true", text, re.M))
    return files, proc_macro


def exempted():
    """The crate versions supply-chain/config.toml exempts."""
    text = open(os.path.join(ROOT, "supply-chain", "config.toml")).read()
    return sorted(set(re.findall(r'\[\[exemptions\.([^\]]+)\]\]\s*\nversion = "([^"]+)"', text)))


def scan(name, version, out):
    roots = [os.path.join(r, f"{name}-{version}") for r in REGISTRIES]
    root = next((r for r in roots if os.path.isdir(r)), None)
    if root is None:
        print(f"not downloaded: {name} {version} (run cargo fetch --locked)", file=sys.stderr)
        return False
    build_files, proc_macro = build_time_files(root)
    # Every folder: one named `target` or `.git` is as much part of the crate as any other.
    for dirpath, dirnames, filenames in os.walk(root):
        for fn in filenames:
            path = os.path.join(dirpath, fn)
            rel = os.path.relpath(path, root)
            try:
                head = open(path, "rb").read(8)
            except OSError:
                continue
            ext = os.path.splitext(fn)[1].lower()
            prebuilt = ext in BINARY_EXT or (ext not in TEXT_EXT and (
                any(head.startswith(m) for m in EXEC_MAGIC) or head[:2] in COFF_MACHINES))
            if prebuilt:
                out.append({"crate": name, "version": version, "file": rel, "kind": "executable", "line": 0, "text": head.hex()})
                continue
            if ext not in TEXT_EXT:
                continue
            build_time = path in build_files or (proc_macro and ext == ".rs") or rel.startswith("build/")
            try:
                lines = open(path, errors="replace").read().split("\n")
            except OSError:
                continue
            for no, line in enumerate(lines, 1):
                for kind, rx in PATTERNS.items():
                    if kind == "url" and not build_time:
                        continue
                    if rx.search(line):
                        out.append({"crate": name, "version": version, "file": rel, "kind": kind,
                                    "build_time": build_time, "line": no, "text": line.strip()[:200]})
                if build_time and ext == ".rs":
                    if BUILD_FS_WRITE.search(line):
                        out.append({"crate": name, "version": version, "file": rel, "kind": "build-fs-write",
                                    "build_time": True, "line": no, "text": line.strip()[:200]})
                    for var in ENV_READ.findall(line):
                        if not HARMLESS_ENV.match(var):
                            out.append({"crate": name, "version": version, "file": rel, "kind": "build-env",
                                        "build_time": True, "line": no, "text": line.strip()[:200]})
    return True


def main():
    findings = []
    missing = [f"{name} {version}" for name, version in exempted() if not scan(name, version, findings)]
    out = sys.argv[1] if len(sys.argv) > 1 else "capability-scan.jsonl"
    with open(out, "w") as f:
        for item in findings:
            f.write(json.dumps(item) + "\n")
    from collections import Counter
    print(Counter((x["kind"], x.get("build_time", False)) for x in findings))
    # A crate not scanned is not a crate found clean.
    if missing:
        print(f"{len(missing)} exempted crates were not scanned", file=sys.stderr)
        sys.exit(1)


main()
