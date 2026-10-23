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

## Owning the server, and its users

- **Users belong to the server**: they are kept in its `accounts.json`, with no account
  anywhere else. It starts with its owner, an admin, named after the system's user it runs as
  and without a password: not adopted yet.
- **Adopting** it is choosing the owner's name and password. On the server's own computer, the
  device paired there does so on the password page. From another computer, the device types
  the connection code, as any new device does, and also proves to the server that it knows
  it, with an HMAC under the same key over a message of its own (`/claim`, asked as often as
  proofs are); only then, and only while nobody has adopted the server, is it given a ticket
  for the page that adopts it. Anyone on the network can reach an unadopted server, but only
  someone who sees its computer's code can own it. A server not adopted keeps a connection
  code, a new one every ten minutes, written to a file only its user reads, for
  `freya-server --code` on a server without a screen; the log says where it is, never what
  it is.
- **Inviting** a user, which admins alone may do, gives a one-time code lasting a week, kept
  by the server only as its key (PBKDF2, as a connection code's). Typed on the user's
  computer, it proves the server, and the device proving it knows it too is given a ticket for
  the page where the user chooses their password, under the name they were invited with,
  whatever is sent. Joining uses it up; inviting a user again replaces it, and lets one who
  joined choose a new password, logging out their devices. A login's code and a password
  page hold only while the password is the one they were given under, checked as they are
  used, so neither given before a password is set, nor during, outlasts it. A ticket lasts no
  longer than the invitation it was proved with, and adopting is checked again as it is
  written, so two devices do not both adopt. Removing a user takes their devices, invitation and tickets away,
  and no id is given twice. The owner is neither invited again nor removed (their password is
  reset on the server's computer), and only the owner invites another admin again or removes
  one, so no admin takes the owner's or another admin's account.
- The server proves itself with each code it knows at once, so a device on the network learns
  how many invitations are pending, nothing more.

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
  second. Tries are counted before a password is checked: per user from a network, per network
  (IPv6 by its /64, link-local addresses one by one), and per user from anywhere (20). Five are
  free, then none is tried for 30 seconds, twice as long after each, up to 15 minutes,
  forgotten a day after. Where a user logged in before, only their own tries there count, from
  their address and their network (20), so a guesser elsewhere never holds them up. A name
  counts by its first 128 characters, and whether it exists does not show: a made-up one is
  counted, waited for and checked against a hash of nothing, as long. The table of tries keeps
  running waits, and with no room takes no new try, but a user's at home.
- **The password** is changed on the server's page, opened with a start that works once, through
  a file only the user can read, so the start is in no command line; the browser that came
  with it is given an HttpOnly, SameSite=Strict cookie, which alone opens the page after. A
  password once set is asked for to change it; changing it logs the other devices out unless
  the user unticks it. One forgotten is forgotten on the server's computer with the server
  stopped, `freya-server --forget-password`, which logs out the devices that logged in with it.
  The login page is opened through such a file too, its state in no command line.
- **The pages** run no script, cannot be framed, and take a form only from their own origin;
  the proxy sets these itself, whatever a server sends.

## Tokens

- **Access tokens** last an hour: the device's id and when it ends, signed by the server
  (HMAC-SHA256 with a 256-bit secret in `accounts.json`). Nothing is kept of them, and a device
  logged out or revoked is refused at once, as each call looks it up.
- **Refresh tokens** renew them, once: each renewal gives a new one. One used before, coming
  again, was copied, and the device is logged out. A renewal whose answer was lost may be asked
  again once, within a day, so a computer that slept through it is not logged out.
- **On the client**: refresh tokens are kept in Windows' Credential Manager and Linux's Secret
  Service, else in a file only the user reads (0600); on macOS in that file until Freya is
  signed with a Developer ID, as an ad hoc signature changes with each version and the
  Keychain would ask for the user's password after every update. Access tokens live in memory.
  Renewals are one at a time, ahead of the end by the clock, and again after a 401. A token's
  lifetime is taken as a day at most, whatever a server says. A token kept in the file when the
  keychain could not take it removes the keychain's old one.

## Roles

An **admin** keeps the server. A **listener** may only make the calls
`freya_protocol::LISTENER_CALLS` names: reading and playing the library everyone shares. A call
not named there is refused, so one added later is too until named. Plays, favourites,
playlists and Last.fm are the owner's until each user has their own, so a listener has none,
and hears only how the shared library goes, not the folders a scan looks in. A listener does
see the tracks' paths, which `/audio` is asked by, and with them where the music is. A
listener is not told when the library changes, which the owner's plays would show. Only a
paired admin stops the server.

## The server under load

Nobody can wear it out before logging in:

- at most 128 connections of strangers, 16 from one address (an IPv6 one by its /64), on each
  of its two listeners: a connection that sent a request with a valid token is a known
  device's, and strangers' do not crowd it out; 512 in all;
- the TLS handshake and a request's head within 10 seconds, its body at 32 KB a second at
  least, an answer taken within 30 seconds and a second more for each 32 KB, each in all,
  inside TLS too, and 15 seconds' wait for the next request, two minutes on a connection a
  known device made;
- as many open files as the system allows, not the 256 macOS gives an agent;
- bodies as large as the route and caller allow, decided before any of it is read: 16 KB
  without a valid token, 8 MB for a call, 64 MB for an admin's picture; read as they come;
- one `Content-Length`, in digits, and no chunked requests;
- only the library's answers compressed, which carry no secret, so the length of a
  compressed answer tells nothing of a token or a key;
- calls asked at once (`/calls`) only those that read the library, at most 256 and 32 MB
  of answers, each allowed to a listener or not on its own, as if asked alone; small
  pictures at most 64 at once, in one size only, made from pictures already kept;
- events wait for a listener in a queue of 1,024, past which it is let go;
- stopping the server, or turning the network off, ends every connection.

## Files

- `/audio` serves only a regular file within the music's folders, as the system says of the
  file opened itself (`F_GETPATH`, `/proc/self/fd`, `GetFinalPathNameByHandleW`), so a link
  swapped in between a check and the opening is caught; it is opened without waiting, so a
  FIFO holds nothing up. A cover is read only for a track of the library, and its image only
  within the track's folder, after links.
- A scan follows no link out of the folders it walks, so one put in a shared music folder does
  not bring the rest of the computer into the library.
- The server's folder holds `accounts.json`, `pairing.key`, `tls-key.pk8`,
  `tls-certificate.der` and `server.log`, all 0600, written as new files and renamed into place.
  Started at login, the server makes files for the user alone (launchd's `Umask`, systemd's
  `UMask=0077`); on Linux its unit gains no privileges, and seccomp lets it make only a
  service's system calls; it retries every ten seconds, never giving up.
- Accounts, the certificate or the key that cannot be read stop the server rather than being
  made anew, which would lose the users or break every device's pin.
- No secret is logged; names lose control characters before they are.

## A client and a server it may not trust

- A library on another computer is always streamed through its server: a path it names is
  never looked for on the client, where it could name a local file or a Windows share. Up Next
  is restored only for the library it was saved from, and emptied when another is chosen.
- A stream a server elsewhere names is asked only of addresses beyond this computer and its
  network: each name is resolved and every near address dropped, a redirect's too, and no
  proxy is used, which would resolve it itself.
- What a server sends is bounded: 1 MB for small answers and each event line, 32 MB for a
  picture, 512 MB for the library's calls, read straight into their types; every picture is
  decoded within 12,000 pixels a side and 256 MB.
- A login's tokens go only with the server that gave them, never to one chosen meanwhile.
- What answers on this computer's address without the user's certificate is given nothing.
- The picture cache keeps and gives only bytes whose SHA-256 is their name. The covers a
  server makes small for a grid cannot be checked so: those of a server elsewhere are kept
  apart, for that server alone, decoded within the same bounds.
- The library's large answers of a server elsewhere are kept on disk for that server alone,
  readable by the user alone, and forgotten when the device logs out.
- The fingerprint shown is the certificate's the client pinned.
- Only HTTPS pages, and Freya's own, are opened, in plain URL characters.

## Not guarded against

- **Another program of the same user** can read the token file, the pairing key and the
  server's key.
- **The login page's origin is shared** by every server's pages (`127.0.0.1:47477`): a
  password manager may offer one server's saved password on another's page. A server is
  logged in to only once its connection code proved it, and its pages run no script, but a
  user who chooses to fill in the password there gives it to that server.
- **A server elsewhere's settings**, its fanart.tv key among them, are kept in the client's own
  settings file while it is shown, for the settings page; an admin's device only.
- **A new connection of a known device** is a stranger's until its first request: while
  strangers from eight addresses hold every stranger's place, it waits, though one open goes
  on.
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
  whole twice by independent reviews: the server's HTTP and TLS, the login and tokens, and what
  the client trusts.

Found something? Write to the maintainers privately, not in a public issue.
