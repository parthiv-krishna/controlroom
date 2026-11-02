//! The app shell's Content-Security-Policy.
//!
//! No inline scripts: `script-src 'self'` also covers lazily loaded same-origin chunks. Later
//! viewers (spec §23) extend it with one [`Csp::add`] per source without touching the base,
//! e.g. a pdf.js worker (`worker-src blob:`), object-URL images (`img-src blob:`) or the
//! separate content origin for sandboxed HTML (`frame-src https://files-….example.com`).
//! Responses that set their own CSP (the reserved `/api/files/` with `sandbox`) keep it.

use crate::origin::OriginPolicy;

/// A CSP as an ordered list of directives.
#[derive(Debug, Clone)]
pub struct Csp {
    directives: Vec<(&'static str, Vec<String>)>,
}

impl Csp {
    /// The app shell policy for `origin`.
    pub fn app(origin: &OriginPolicy) -> Self {
        let sources = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        // Older Safari doesn't count ws(s): as 'self'.
        let mut connect = sources(&["'self'"]);
        connect.push(origin.websocket_origin());
        Self {
            directives: vec![
                ("default-src", sources(&["'self'"])),
                ("script-src", sources(&["'self'"])),
                // Svelte sets style attributes and transitions insert keyframes.
                ("style-src", sources(&["'self'", "'unsafe-inline'"])),
                ("img-src", sources(&["'self'", "data:"])),
                ("font-src", sources(&["'self'"])),
                ("connect-src", connect),
                ("worker-src", sources(&["'self'"])),
                ("manifest-src", sources(&["'self'"])),
                ("frame-src", sources(&["'none'"])),
                ("object-src", sources(&["'none'"])),
                ("base-uri", sources(&["'none'"])),
                ("form-action", sources(&["'self'"])),
                ("frame-ancestors", sources(&["'none'"])),
            ],
        }
    }

    /// Adds `source` to `directive`, replacing a lone `'none'`.
    pub fn add(mut self, directive: &'static str, source: &str) -> Self {
        match self.directives.iter_mut().find(|(d, _)| *d == directive) {
            Some((_, sources)) => {
                sources.retain(|s| s != "'none'");
                if !sources.iter().any(|s| s == source) {
                    sources.push(source.to_owned());
                }
            }
            None => self.directives.push((directive, vec![source.to_owned()])),
        }
        self
    }

    /// The header value.
    pub fn render(&self) -> String {
        self.directives
            .iter()
            .map(|(directive, sources)| format!("{directive} {}", sources.join(" ")))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

#[cfg(test)]
mod tests {
    use url::Url;

    use super::*;

    #[test]
    fn no_inline_scripts_and_extensible() {
        let origin = OriginPolicy::from_public_url(&Url::parse("https://cr.example.com").unwrap());
        let csp = Csp::app(&origin).render();
        assert!(csp.contains("script-src 'self';"), "{csp}");
        assert!(!csp.contains("unsafe-eval"));
        assert!(csp.contains("connect-src 'self' wss://cr.example.com"));
        assert!(csp.contains("frame-ancestors 'none'"));
        let viewer = Csp::app(&origin)
            .add("worker-src", "blob:")
            .add("frame-src", "https://files.example.com")
            .add("img-src", "blob:")
            .render();
        assert!(viewer.contains("worker-src 'self' blob:"));
        assert!(viewer.contains("frame-src https://files.example.com;"));
        assert!(viewer.contains("img-src 'self' data: blob:"));
        assert!(viewer.contains("script-src 'self';"));
    }
}
