# Security

How Freya's server and its clients keep the library, the account and the computers they run on
safe: what they guard against, how, and what they do not. Kept up to date with the code; the
milestones that made it are in [PLAN.md](PLAN.md) (A1–A4, S1–S7).

## What is guarded, and from whom

Freya's server keeps the user's library, pictures, playlists, plays and Last.fm account, and
reads the music's folders. Its clients play the music and show the library. What matters:

- **The account**: the password, the devices' tokens, and with them whatever an admin may do:
  change the server's settings, choose folders, see the server's folders, scan.
- **The computer the server runs on**: no file outside the music may be read through it.
- **The computers the clients run on**: a server may be hostile, or answer in another's place.

Who may attack:

| Who | Where | Can |
|---|---|---|
| A device on the home network | LAN | Reach the server's TLS port and discovery; send anything |
| A web page in the user's browser | Browser | Send requests to `127.0.0.1` and to the network, without reading answers |
| Another user of the same computer | Same computer | Listen on ports before Freya does, see command lines, reach `127.0.0.1` |
| A hostile server | The network | Answer a client anything, once it was chosen |
| Someone who learned the password | Anywhere | Log in, until the password changes |

A program of the same user on the same computer is not guarded against: it can read the user's
files, and with them Freya's.

## The connection

- **Over TLS everywhere**, TLS 1.3 only (rustls). The server's certificate is its own,
  self-signed (ECDSA P-256), named `freya-server.invalid` whatever its address, and never
  expires: a client trusts it because it pinned it.
- **The same computer**: the client reaches the server on `127.0.0.1:47474` over TLS too,
  pinned to the certificate in the server's folder, which only the user can write. A program
  of another user listening there first is found out and given nothing; Freya says so. A
  server finding its address held checks whether it is the user's own before leaving quietly.
- **Another computer**: found by UDP discovery (port 47476) or typed in, then proved with a
  **connection code** shown on the server's computer (Settings, Server, Add a Device): ten
  characters (50 bits), for ten minutes. The server proves it knows the code with an
  HMAC-SHA256 keyed by PBKDF2 of the code (600,000 rounds, salted with the certificate's
  SHA-256) over the client's nonce and the certificate the client was given. Another computer
  in its place cannot; one passing the question on gets a proof for the wrong certificate, and
  searching a proof for the code takes thousands of years while it lasts minutes. Only then
  can the server be logged in to. A server the computer used before is known by its
  certificate at any address.
- **Discovery** answers only private, link-local and loopback addresses, each at most twice a
  second, with a short answer, so the server cannot be used to flood another computer. A
  server found that gives another certificate than it named is refused.

## Logging in

- **On the same computer**, the client pairs with the pairing key the server writes to a file
  only the user can read (`pairing.key`, 0600), made anew when used. The key is never sent: an
  HMAC with it over a nonce is. A wrong one changes nothing and holds the answer up a tenth of
  a second.
- **Elsewhere**, in the browser, so a password manager fills it in: OAuth 2.0's authorization
  code with PKCE (S256). Freya opens the server's login page through a proxy on
  `http://127.0.0.1:47477`, which carries it to the server over the pinned connection. The page
  sends the browser back to `http://127.0.0.1:47477/callback`, the only place a code is sent,
  with a code that lasts two minutes and is taken once; only the client that sent the challenge
  trades it, with the secret it was made from, for tokens.
- **Passwords**: PBKDF2-HMAC-SHA256, 600,000 rounds, salted. A wrong one holds the answer up a
  second. Tries are counted before a password is checked, per user from a network, per network
  (IPv6 by its /64), and per user from anywhere (20): five are free, then none is tried for
  30 seconds, twice as long after each, up to 15 minutes, forgotten a day after. A user is not
  held up where they logged in before. Whether a name exists does not show: a made-up one is
  counted, waited for and checked against a hash of nothing, as long.
- **The password** is changed on the server's page, opened with a start that works once; the
  browser that came with it is given an HttpOnly, SameSite=Strict cookie, which alone opens the
  page after. A password once set is asked for to change it; changing it may log the other
  devices out. One forgotten is forgotten on the server's computer with the server stopped:
  `freya-server --forget-password`.
- **The pages** run no script, cannot be framed, and take a form only from their own origin;
  the proxy sets these itself, whatever a server sends.

## Tokens

- **Access tokens** last an hour: the device's id and when it ends, signed by the server
  (HMAC-SHA256 with a 256-bit secret in `accounts.json`). Nothing is kept of them, and a device
  logged out or revoked is refused at once, as each call looks it up.
- **Refresh tokens** renew them, once: each renewal gives a new one. One used before, coming
  again, was copied, and the device is logged out. A renewal whose answer was lost may be asked
  again once, for two minutes.
- **On the client**: refresh tokens are kept in Windows' Credential Manager and Linux's Secret
  Service, else in a file only the user reads (0600); on macOS in that file until Freya is
  signed with a Developer ID, as an ad hoc signature changes with each version and the
  Keychain would ask for the user's password after every update. Access tokens live in memory.
  Renewals are one at a time, ahead of the end by the clock, and again after a 401.

## Roles

An **admin** keeps the server. A **listener** may only make the calls
`freya_protocol::LISTENER_CALLS` names: reading and playing the library everyone shares. A call
not named there is refused, so one added later is too until named. Plays, favourites,
playlists and Last.fm are the owner's until each user has their own, so a listener has none,
and hears only how the shared library goes, not where its folders are. Only a paired admin
stops the server.

## The server under load

Nobody can wear it out before logging in:

- at most 128 connections, 16 from one address;
- a request's head within 10 seconds, its body at 32 KB a second at least, an answer taken
  within 30, and 15 seconds' wait for the next request;
- bodies as large as the route and caller allow, decided before any of it is read: 16 KB
  without a valid token, 8 MB for a call, 64 MB for an admin's picture; read as they come;
- one `Content-Length`, in digits, and no chunked requests;
- events wait for a listener in a queue of 1,024, past which it is let go;
- stopping the server, or turning the network off, ends every connection.

## Files

- `/audio` and covers serve only regular files within the music's folders, after links: a link
  put in a shared music folder, pointing at the server's key, is not followed out.
- The server's folder holds `accounts.json`, `pairing.key`, `tls-key.pk8`,
  `tls-certificate.der` and `server.log`, all 0600, written as new files and renamed into place.
  Started at login, the server makes files for the user alone (launchd's `Umask`, systemd's
  `UMask=0077`); on Linux its unit gains no privileges and is narrowed by seccomp.
- Accounts, the certificate or the key that cannot be read stop the server rather than being
  made anew, which would lose the users or break every device's pin.
- No secret is logged; names lose control characters before they are.

## A client and a server it may not trust

- A library on another computer is always streamed through its server: a path it names is
  never looked for on the client, where it could name a local file or a Windows share.
- What a server sends is bounded: 1 MB for small answers and each event line, 32 MB for a
  picture, 512 MB for the library's calls; pictures are decoded within 12,000 pixels a side
  and 256 MB.
- The picture cache keeps and gives only bytes whose SHA-256 is their name.
- The fingerprint shown is the certificate's the client pinned.
- Only HTTPS pages, and Freya's own, are opened, in plain URL characters.

## Not guarded against

- **Another program of the same user** can read the token file, the pairing key and the
  server's key.
- **The login page's origin is shared** by every server's pages (`127.0.0.1:47477`): a
  password manager may offer one server's saved password on another's page. A server is
  logged in to only once its connection code proved it, and its pages run no script, but a
  user who chooses to fill in the password there gives it to that server.
- **A server reached from the internet** has not been the goal so far: discovery and the
  connection code assume a home network, and the server's own certificate is not one browsers
  trust.
- **Denial of service by many computers at once**: the limits above bound what one address or
  network can hold, not a crowd.

## Checking it

- `cargo test` runs hostile input against the API's routes and pages, called as the server
  calls them (20,000 requests a stranger might send, none let in), against the HTTP server over
  the network, and against the parsers of ranges, escapes and forms.
- Every commit and release is scanned for secrets with gitleaks; dependencies are checked with
  cargo-deny and cargo-vet (see [DEPENDENCIES.md](DEPENDENCIES.md)).
- The milestones were each reviewed by an automatic security review of the commits, and the
  whole by independent reviews: the server's HTTP and TLS, the login and tokens, and what the
  client trusts.

Found something? Write to the maintainers privately, not in a public issue.
