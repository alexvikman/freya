//! The pages a browser opens on the server, under [`PAGES_PATH`], so a password manager fills
//! them in and keeps what is typed:
//!
//! - `GET /login`, with OAuth's `response_type=code`, `redirect_uri`, `state`, `code_challenge`
//!   and `code_challenge_method=S256`: a form for the user and their password. `POST /login`
//!   sends the browser on to `redirect_uri` with a code and the state, which the client that
//!   asked trades for a token at the API's `/token`. Only a client on the browser's own
//!   computer is sent to: `redirect_uri` is `http://127.0.0.1` or `http://[::1]`.
//! - `GET /logged-in`: where the client sends the browser once it has its token.
//! - `GET /password?ticket=`: a form for a new password, and the one now unless the ticket is
//!   of a device on the server's computer. `POST /password` changes it.
//!
//! A page is answered only through this computer's name over plain HTTP, as the API is, and a
//! form only when sent from the page's own origin. Nothing on a page is scripted, and no other
//! site may frame it.

use freya_protocol::PAGES_PATH;

use crate::accounts::{Accounts, SHORTEST_PASSWORD};
use crate::http::{Request, Response};
use crate::serve::{Served, found, is_here, percent_decode};

/// The page `request` asks for; none when it is not one of these.
pub fn answer(served: &Served, request: &Request) -> Option<Response> {
    let path = request.path.strip_prefix(PAGES_PATH)?;
    if !request.secure && !is_here(request) {
        return Some(message(403, "Not Here", "Open this page from Freya."));
    }
    let Some(accounts) = &served.accounts else {
        return Some(message(404, "No Such Page", "This server takes anyone."));
    };
    if request.method == "POST" {
        if let Some(origin) = request.header("origin") {
            let scheme = if request.secure { "https" } else { "http" };
            let own = format!("{scheme}://{}", request.header("host").unwrap_or_default());
            if origin != own {
                return Some(message(
                    403,
                    "Not Sent From Here",
                    "Open this page from Freya.",
                ));
            }
        }
        let form = request
            .header("content-type")
            .and_then(|kind| kind.split(';').next())
            .is_some_and(|kind| kind.trim() == "application/x-www-form-urlencoded");
        if !form {
            return Some(message(400, "Not a Form", "Open this page from Freya."));
        }
    }
    let server = found(served).name;
    Some(match (request.method.as_str(), path) {
        ("GET", "/login") => {
            let values = values(&request.query);
            match Login::asked(&values) {
                Ok(login) => login_page(200, &server, &login, "", ""),
                Err(why) => message(400, "Freya Cannot Log In Here", why),
            }
        }
        ("POST", "/login") => {
            let values = values(&String::from_utf8_lossy(&request.body));
            match Login::asked(&values) {
                Ok(login) => logged_in(accounts, &server, &login, &values),
                Err(why) => message(400, "Freya Cannot Log In Here", why),
            }
        }
        ("GET", "/logged-in") => message(
            200,
            "You Are Logged In",
            &format!("Freya uses the library on {server} now. You can close this page."),
        ),
        ("GET", "/password") => {
            let values = values(&request.query);
            let ticket = value(&values, "ticket");
            password_page(accounts, 200, &server, ticket, "")
        }
        ("POST", "/password") => {
            let values = values(&String::from_utf8_lossy(&request.body));
            let ticket = value(&values, "ticket");
            let (new, again) = (value(&values, "password"), value(&values, "again"));
            if new != again {
                password_page(
                    accounts,
                    400,
                    &server,
                    ticket,
                    "The passwords are not the same.",
                )
            } else {
                match accounts.change_password(ticket, value(&values, "current"), new) {
                    Ok(()) => message(
                        200,
                        "The Password Is Changed",
                        "Other devices log in with it from now on. You can close this page.",
                    ),
                    Err(why) => password_page(accounts, 400, &server, ticket, &why),
                }
            }
        }
        _ => message(404, "No Such Page", "Open this page from Freya."),
    })
}

/// A login asked for: where to send the browser after, with what state, for the client that
/// sent the challenge.
struct Login {
    redirect: String,
    state: String,
    challenge: String,
}

impl Login {
    fn asked(values: &[(String, String)]) -> Result<Self, &'static str> {
        if value(values, "response_type") != "code"
            || value(values, "code_challenge_method") != "S256"
        {
            return Err("Freya asked for a kind of login this server does not give.");
        }
        let challenge = value(values, "code_challenge");
        let encoded = challenge
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if !(43..=128).contains(&challenge.len()) || !encoded {
            return Err("Freya sent a challenge this server does not take.");
        }
        let redirect = value(values, "redirect_uri");
        if !is_loopback(redirect) {
            return Err("Logging in sends you back to Freya on this computer only.");
        }
        let state = value(values, "state");
        if state.len() > 200 {
            return Err("Freya sent too long a state.");
        }
        Ok(Self {
            redirect: redirect.to_owned(),
            state: state.to_owned(),
            challenge: challenge.to_owned(),
        })
    }
}

/// The browser sent on to the client with a code, given the user's password; the form again
/// when it is wrong.
fn logged_in(
    accounts: &Accounts,
    server: &str,
    login: &Login,
    values: &[(String, String)],
) -> Response {
    let user = value(values, "username").trim();
    let granted = accounts.grant(
        user,
        value(values, "password"),
        &login.challenge,
        &login.redirect,
    );
    let Some(code) = granted else {
        return login_page(401, server, login, user, "Wrong user or password.");
    };
    let joined = if login.redirect.contains('?') {
        '&'
    } else {
        '?'
    };
    let to = format!(
        "{}{joined}code={code}&state={}",
        login.redirect,
        percent_encode(&login.state)
    );
    let mut sent = html(303, "");
    sent.headers.push(("Location", to));
    sent
}

fn login_page(status: u16, server: &str, login: &Login, user: &str, error: &str) -> Response {
    let hidden = [
        ("response_type", "code"),
        ("code_challenge_method", "S256"),
        ("code_challenge", login.challenge.as_str()),
        ("redirect_uri", login.redirect.as_str()),
        ("state", login.state.as_str()),
    ]
    .iter()
    .map(|(name, value)| {
        format!(
            r#"<input type="hidden" name="{name}" value="{}">"#,
            escape(value)
        )
    })
    .collect::<String>();
    let title = format!("Log In to {server}");
    let body = format!(
        r#"<h1>{title}</h1>
<p>Freya on this computer uses the library on {server} once you have.</p>
<form method="post" action="{PAGES_PATH}/login">{hidden}
<label>User<input name="username" autocomplete="username" autocapitalize="none" spellcheck="false" required{focus_user} value="{user}"></label>
<label>Password<input name="password" type="password" autocomplete="current-password" required{focus_password}></label>
{error}<button>Log In</button>
</form>"#,
        title = escape(&title),
        server = escape(server),
        user = escape(user),
        focus_user = if user.is_empty() { " autofocus" } else { "" },
        focus_password = if user.is_empty() { "" } else { " autofocus" },
        error = error_line(error),
    );
    html(status, &document(&title, &body))
}

fn password_page(
    accounts: &Accounts,
    status: u16,
    server: &str,
    ticket: &str,
    error: &str,
) -> Response {
    let Some(page) = accounts.password_page(ticket) else {
        return message(
            404,
            "This Page Is Out of Date",
            "Open it again from Freya's settings.",
        );
    };
    let current = if page.asks_current {
        r#"<label>Password now<input name="current" type="password" autocomplete="current-password" required autofocus></label>"#
    } else {
        ""
    };
    let title = "Your Password";
    let body = format!(
        r#"<h1>{title}</h1>
<p>{user} logs in to the library on {server} with it, from other devices. At least {SHORTEST_PASSWORD} characters.</p>
<form method="post" action="{PAGES_PATH}/password">
<input type="hidden" name="ticket" value="{ticket}">
<label>User<input name="username" autocomplete="username" value="{user}" readonly></label>
{current}
<label>New password<input name="password" type="password" autocomplete="new-password" minlength="{SHORTEST_PASSWORD}" required{focus}></label>
<label>Again<input name="again" type="password" autocomplete="new-password" minlength="{SHORTEST_PASSWORD}" required></label>
{error}<button>Save</button>
</form>"#,
        user = escape(&page.user),
        server = escape(server),
        ticket = escape(ticket),
        focus = if page.asks_current { "" } else { " autofocus" },
        error = error_line(error),
    );
    html(status, &document(title, &body))
}

/// A page that only says something.
fn message(status: u16, title: &str, text: &str) -> Response {
    let body = format!("<h1>{}</h1>\n<p>{}</p>", escape(title), escape(text));
    html(status, &document(title, &body))
}

fn error_line(error: &str) -> String {
    if error.is_empty() {
        String::new()
    } else {
        format!("<p class=\"error\" role=\"alert\">{}</p>\n", escape(error))
    }
}

/// A page, with the headers that keep scripts, frames and other sites out of it.
fn html(status: u16, page: &str) -> Response {
    let mut response = Response::bytes(status, "text/html; charset=utf-8", page.into());
    response.headers.extend([
        (
            "Content-Security-Policy",
            "default-src 'none'; style-src 'unsafe-inline'; form-action 'self' http://127.0.0.1:* http://[::1]:*; frame-ancestors 'none'; base-uri 'none'".into(),
        ),
        ("X-Frame-Options", "DENY".into()),
        ("X-Content-Type-Options", "nosniff".into()),
        ("Referrer-Policy", "same-origin".into()),
        ("Cache-Control", "no-store".into()),
    ]);
    response
}

/// A whole page in Freya's colors, light or dark as the system is.
fn document(title: &str, body: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} · Freya</title>
<style>
:root {{ color-scheme: light dark; --background: #f6f7f9; --surface: #ffffff; --text: #121418; --secondary: #4b5360; --divider: #e1e4e9; --accent: #a35f00; --on-accent: #ffffff; --danger: #b3261e; }}
@media (prefers-color-scheme: dark) {{ :root {{ --background: #15171a; --surface: #1f2226; --text: #f3f5f8; --secondary: #b9c0ca; --divider: #2b2f35; --accent: #ffb22e; --on-accent: #1a1406; --danger: #ff8a80; }} }}
* {{ box-sizing: border-box; }}
body {{ margin: 0; min-height: 100vh; display: grid; place-items: center; padding: 24px 16px; background: var(--background); color: var(--text); font: 15px/1.45 system-ui, -apple-system, "Segoe UI", sans-serif; }}
main {{ width: 100%; max-width: 400px; background: var(--surface); border: 1px solid var(--divider); border-radius: 12px; padding: 28px; }}
h1 {{ margin: 0 0 6px; font-size: 22px; }}
p {{ margin: 0 0 18px; color: var(--secondary); }}
form {{ display: grid; gap: 14px; }}
label {{ display: grid; gap: 6px; font-size: 13px; font-weight: 600; }}
input {{ font: inherit; padding: 9px 11px; border: 1px solid var(--divider); border-radius: 8px; background: var(--background); color: var(--text); }}
input:focus {{ outline: 2px solid var(--accent); outline-offset: -1px; }}
input[readonly] {{ color: var(--secondary); }}
button {{ font: inherit; font-weight: 600; padding: 10px; border: 0; border-radius: 8px; background: var(--accent); color: var(--on-accent); cursor: pointer; }}
.error {{ margin: 0; color: var(--danger); }}
</style>
</head>
<body>
<main>
{body}
</main>
</body>
</html>
"#,
        title = escape(title),
    )
}

/// Whether `uri` leads to this computer by its address: `http://127.0.0.1:port/...` or
/// `http://[::1]:port/...`.
fn is_loopback(uri: &str) -> bool {
    let Some(rest) = uri.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split(['/', '?']).next().unwrap_or_default();
    let Some((host, port)) = authority.rsplit_once(':') else {
        return false;
    };
    ["127.0.0.1", "[::1]"].contains(&host) && port.parse::<u16>().is_ok() && !uri.contains('#')
}

/// The pairs of a query or form: `a=1&b=x+y`, its `+` spaces and its escapes undone.
fn values(encoded: &str) -> Vec<(String, String)> {
    encoded
        .split('&')
        .filter_map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            let decode = |text: &str| percent_decode(&text.replace('+', " "));
            Some((decode(name)?, decode(value)?))
        })
        .collect()
}

fn value<'a>(values: &'a [(String, String)], name: &str) -> &'a str {
    values
        .iter()
        .find(|(n, _)| n == name)
        .map_or("", |(_, v)| v.as_str())
}

/// `text` in a URL's query, all but its unreserved characters escaped.
fn percent_encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// `text` in HTML, as text or an attribute's value.
fn escape(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '&' => "&amp;".to_owned(),
            '<' => "&lt;".to_owned(),
            '>' => "&gt;".to_owned(),
            '"' => "&quot;".to_owned(),
            '\'' => "&#39;".to_owned(),
            c => c.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sends_back_to_this_computer_only() {
        assert!(is_loopback("http://127.0.0.1:47477/callback"));
        assert!(is_loopback("http://[::1]:5000/"));
        assert!(!is_loopback("https://127.0.0.1:47477/callback"));
        assert!(!is_loopback("http://127.0.0.1.evil.example:80/"));
        assert!(!is_loopback("http://evil.example@127.0.0.1:80/"));
        assert!(!is_loopback("http://localhost:47477/callback"));
        assert!(!is_loopback("http://127.0.0.1/callback"));
        assert!(!is_loopback("http://127.0.0.1:47477/callback#x"));
    }

    #[test]
    fn reads_forms_and_escapes_pages() {
        let read = values("username=al+ex&password=a%26b%3D&empty&state=%E2%9C%93");
        assert_eq!(value(&read, "username"), "al ex");
        assert_eq!(value(&read, "password"), "a&b=");
        assert_eq!(value(&read, "empty"), "");
        assert_eq!(value(&read, "state"), "✓");
        assert_eq!(value(&read, "missing"), "");
        assert_eq!(
            escape(r#"<a href="x">'&'</a>"#),
            "&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;"
        );
        assert_eq!(percent_encode("a b&c=✓"), "a%20b%26c%3D%E2%9C%93");
    }
}
