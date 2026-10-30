//! Login/SSO-wall detection shared by the HTTP crawler and the browser
//! adapters. Pure functions over URLs and HTML only — nothing here touches
//! cookies, tokens, or form values.

use url::Url;

use crate::schema::AuthRequiredInfo;

/// Below this much visible text, a password field means the page *is* the
/// login wall rather than a content page with a navbar login widget.
const THIN_PAGE_CHARS: usize = 500;

const SSO_HOST_MARKERS: &[&str] = &[
    "login.",
    "sso.",
    "auth.",
    "accounts.",
    "signin.",
    "okta.",
    "onelogin.",
    "auth0.",
    "microsoftonline.com",
];

pub fn host_of(url: &str) -> Option<String> {
    Url::parse(url)
        .ok()?
        .host_str()
        .map(|h| h.to_ascii_lowercase())
}

/// `false` when either URL fails to parse (never flag on bad input).
pub fn host_changed(requested: &str, final_url: &str) -> bool {
    match (host_of(requested), host_of(final_url)) {
        (Some(a), Some(b)) => a != b,
        _ => false,
    }
}

/// Crude visible-text length: drops script/style bodies and tags.
pub fn visible_text_len(html: &str) -> usize {
    let lower = html.to_ascii_lowercase();
    let mut out = 0usize;
    let mut i = 0usize;
    let bytes = html.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'<' {
            let rest = &lower[i..];
            let skip_to = ["<script", "<style"].iter().find_map(|open| {
                rest.starts_with(open).then(|| {
                    let close = if *open == "<script" {
                        "</script>"
                    } else {
                        "</style>"
                    };
                    rest.find(close)
                        .map(|p| p + close.len())
                        .unwrap_or(rest.len())
                })
            });
            if let Some(n) = skip_to {
                i += n;
                continue;
            }
            match rest.find('>') {
                Some(p) => i += p + 1,
                None => break,
            }
        } else {
            if !bytes[i].is_ascii_whitespace() {
                out += 1;
            }
            i += 1;
        }
    }
    out
}

fn has_password_field(lower_html: &str) -> bool {
    lower_html.contains("type=\"password\"")
        || lower_html.contains("type='password'")
        || lower_html.contains("type=password")
}

fn title_says_login(lower_html: &str) -> bool {
    let Some(start) = lower_html.find("<title") else {
        return false;
    };
    let rest = &lower_html[start..];
    let end = rest.find("</title>").unwrap_or(rest.len().min(300));
    let title = &rest[..end];
    [
        "sign in",
        "log in",
        "login",
        "single sign-on",
        "authenticate",
    ]
    .iter()
    .any(|m| title.contains(m))
}

fn sso_like_host(host: &str) -> bool {
    SSO_HOST_MARKERS
        .iter()
        .any(|m| host.starts_with(m) || host.contains(m))
}

fn info(requested: &str, final_url: &str) -> AuthRequiredInfo {
    AuthRequiredInfo {
        requested_url: requested.to_string(),
        final_url: final_url.to_string(),
        auth_host: host_of(final_url)
            .or_else(|| host_of(requested))
            .unwrap_or_default(),
        hint: None,
    }
}

/// A 401/403 response to `requested` demands authentication.
pub fn from_status(requested: &str, status: u16, final_url: &str) -> Option<AuthRequiredInfo> {
    matches!(status, 401 | 403).then(|| info(requested, final_url))
}

/// Composite check for a page that loaded successfully: a login marker
/// (password field / login-ish title) on a thin page or after a cross-host
/// redirect, or a cross-host redirect onto an SSO-looking hostname.
pub fn detect_auth_wall(requested: &str, final_url: &str, html: &str) -> Option<AuthRequiredInfo> {
    let lower = html.to_ascii_lowercase();
    let changed = host_changed(requested, final_url);
    // A login-ish title alone is too weak ("Login help"); it must come with a form.
    let marker =
        has_password_field(&lower) || (title_says_login(&lower) && lower.contains("<form"));
    let thin = visible_text_len(html) < THIN_PAGE_CHARS;
    let sso_host = changed && host_of(final_url).is_some_and(|h| sso_like_host(&h));
    ((marker && (changed || thin)) || sso_host).then(|| info(requested, final_url))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOGIN: &str = r#"<html><head><title>Sign in</title></head><body><form><input type="password"></form></body></html>"#;

    #[test]
    fn host_change_same_host_false_other_true_bad_input_false() {
        assert!(!host_changed("http://a.com/x", "https://A.com/y"));
        assert!(host_changed("https://docs.a.com/x", "https://idp.b.com/l"));
        assert!(!host_changed("not a url", "https://b.com"));
    }

    #[test]
    fn cross_host_login_page_is_auth_wall() {
        let i = detect_auth_wall("https://docs.a.com/x", "https://idp.b.com/login", LOGIN).unwrap();
        assert_eq!(i.auth_host, "idp.b.com");
        assert_eq!(i.requested_url, "https://docs.a.com/x");
        assert_eq!(i.final_url, "https://idp.b.com/login");
    }

    #[test]
    fn same_host_thin_login_page_is_auth_wall() {
        assert!(detect_auth_wall("https://a.com/x", "https://a.com/login", LOGIN).is_some());
    }

    #[test]
    fn same_host_content_page_with_navbar_password_field_is_not() {
        let body = "word ".repeat(400);
        let html = format!("<html><body><input type=\"password\"><p>{body}</p></body></html>");
        assert!(detect_auth_wall("https://a.com/x", "https://a.com/x", &html).is_none());
    }

    #[test]
    fn cdn_redirect_without_markers_is_not() {
        let html = "<html><body><p>hello</p></body></html>";
        assert!(detect_auth_wall("https://docs.a.com", "https://a.netlify.app", html).is_none());
    }

    #[test]
    fn sso_hostname_redirect_is_auth_wall_even_without_markers() {
        let html = "<html><body><div id=app></div></body></html>";
        assert!(detect_auth_wall("https://wiki.a.com", "https://a.okta.com/app", html).is_some());
    }

    #[test]
    fn thin_page_titled_login_without_form_is_not_auth_wall() {
        let html = "<html><head><title>Login help</title></head><body><p>Forgot how to sign in? Call IT.</p></body></html>";
        assert!(detect_auth_wall("https://a.com/x", "https://a.com/help", html).is_none());
    }

    #[test]
    fn login_titled_page_with_form_is_auth_wall() {
        let html = "<html><head><title>Log in</title></head><body><form><input name=u></form></body></html>";
        assert!(detect_auth_wall("https://a.com/x", "https://a.com/login", html).is_some());
    }

    #[test]
    fn status_401_403_only() {
        assert!(from_status("https://a.com", 401, "https://a.com").is_some());
        assert!(from_status("https://a.com", 403, "https://a.com").is_some());
        assert!(from_status("https://a.com", 404, "https://a.com").is_none());
    }

    #[test]
    fn visible_text_ignores_script_style_and_tags() {
        assert_eq!(
            visible_text_len("<style>x{}</style><script>var a=1;</script><p>a b</p>"),
            2
        );
    }
}
