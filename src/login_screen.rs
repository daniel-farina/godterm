#[derive(Debug, Clone, Default, PartialEq)]
pub struct LoginScreen {
    pub url: Option<String>,

    pub local_redirect: bool,

    pub user_code: Option<String>,

    pub wants_code: bool,

    pub success: bool,

    pub token_created: bool,

    pub error: Option<String>,

    pub errors: usize,

    pub retry: bool,

    pub onboarding: Option<&'static str>,
}

fn is_error_line(l: &str) -> bool {
    let l = l.to_lowercase();
    l.contains("oauth error")
        || l.contains("login failed")
        || l.contains("invalid code")
        || l.contains("authorization failed")
        || l.contains("request failed with status")
        || l.contains("error: ")
}

fn code_in_url(u: &str) -> Option<String> {
    let q = u.split_once('?')?.1;
    q.split('&')
        .find_map(|kv| kv.strip_prefix("user_code="))
        .map(|c| c.to_string())
        .filter(|c| !c.is_empty() && c.len() <= 32)
}

pub fn read(screen: &str) -> LoginScreen {
    let low = screen.to_lowercase();
    let url = crate::admin::url_on_screen(screen);
    let local_redirect = url.as_deref().is_some_and(|u| {
        let l = u.to_lowercase();
        l.contains("redirect_uri=http%3a%2f%2flocalhost")
            || l.contains("redirect_uri=http%3a%2f%2f127.0.0.1")
            || l.starts_with("http://localhost")
    });
    let user_code = url.as_deref().and_then(code_in_url);
    let errs: Vec<&str> = screen.lines().filter(|l| is_error_line(l)).collect();
    let token_created = low.contains("long-lived authentication token created");
    LoginScreen {
        local_redirect,
        user_code,
        wants_code: low.contains("paste code here"),
        success: token_created
            || low.contains("login successful")
            || low.contains("logged in as")
            || low.contains("successfully logged in")
            || low.contains("successfully signed in"),
        token_created,
        error: errs.last().map(|l| l.trim().to_string()),
        errors: errs.len(),
        retry: low.contains("press enter to retry"),
        onboarding: if low.contains("choose the text style") {
            Some("theme")
        } else if low.contains("select login method") {
            Some("method")
        } else {
            None
        },
        url,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(name: &str) -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/login")
                .join(name),
        )
        .unwrap()
    }

    #[test]
    fn claude_setup_token_waits_for_a_code() {
        let s = read(&fx("claude_setup_token.txt"));
        let u = s.url.unwrap();
        assert!(
            u.starts_with("https://claude.com/cai/oauth/authorize?code=true"),
            "{u}"
        );

        assert!(u.contains("&response_type=code&redirect_uri="), "{u}");
        assert!(u.contains("scope=user%3Ainference&code_challenge="), "{u}");
        assert!(u.contains("&state="), "{u}");
        assert!(!u.contains(' '));
        assert!(!s.local_redirect, "the code comes back on the page");
        assert!(s.wants_code && !s.success && s.error.is_none());
    }

    #[test]
    fn claude_setup_token_bad_code_offers_a_retry() {
        let s = read(&fx("claude_setup_token_bad_code.txt"));
        assert!(s.retry);
        assert_eq!(s.errors, 1);
        assert!(s.error.unwrap().contains("400"));
        assert!(!s.success);
    }

    #[test]
    fn claude_setup_token_success_is_seen_on_the_masked_screen() {
        let text = fx("claude_setup_token_success_masked.txt");
        assert!(!text.contains("sk-ant-oat"), "the fixture holds no token");
        let s = read(&text);
        assert!(s.success && s.token_created);
    }

    #[test]
    fn claude_auth_login_url_starts_mid_line() {
        let s = read(&fx("claude_auth_login.txt"));
        let u = s.url.unwrap();
        assert!(
            u.starts_with("https://claude.com/cai/oauth/authorize?"),
            "{u}"
        );
        assert!(
            u.contains("user%3Ainference") && u.contains("&state="),
            "{u}"
        );
        assert!(s.wants_code);
    }

    #[test]
    fn grok_device_login_shows_its_code() {
        let s = read(&fx("grok_login_device.txt"));
        assert_eq!(
            s.url.as_deref(),
            Some("https://accounts.x.ai/oauth2/device?user_code=FK47-5647")
        );
        assert_eq!(s.user_code.as_deref(), Some("FK47-5647"));
        assert!(!s.wants_code && !s.local_redirect);
    }

    #[test]
    fn codex_login_must_be_opened_on_the_mac() {
        let s = read(&fx("codex_login.txt"));
        let u = s.url.unwrap();
        assert!(
            u.starts_with("https://auth.openai.com/oauth/authorize?"),
            "{u}"
        );
        assert!(u.ends_with("originator=codex_cli_rs"), "{u}");
        assert!(s.local_redirect);
    }

    #[test]
    fn onboarding_and_success_words() {
        assert_eq!(read("Choose the text style").onboarding, Some("theme"));
        assert_eq!(read("Select login method:").onboarding, Some("method"));
        assert!(read("Login successful. Press Enter to continue").success);
        assert!(read("").url.is_none());
    }
}
