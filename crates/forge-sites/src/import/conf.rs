//! Readers for the virtual host files Laragon generates: Apache (`etc/apache2/
//! sites-enabled/auto.*.conf`) and Nginx (`etc/nginx/sites-enabled/*.conf`).
//!
//! Only what a site needs is read: the primary name, the aliases, the document root and
//! whether TLS is served. Nothing is executed or included. Everything else in the file
//! (`<Directory>`, `ProxyPass`, `fastcgi_*`) is ignored: Forgebox renders its own
//! configuration from the site model.
//!
//! Apache: `Define NAME value` and `${NAME}` (Laragon's auto vhosts are written as
//! `define ROOT "C:/laragon/www/app"` / `ServerName ${SITE}`) are expanded; blocks with
//! the same `ServerName` (the `*:80` and `*:443` pair) are merged. Directive names are
//! case-insensitive; names and paths keep the case written.

use std::collections::BTreeMap;

/// A virtual host read from a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vhost {
    /// Primary name as written (case preserved).
    pub server_name: String,
    /// Other names, without wildcards, as written.
    pub aliases: Vec<String>,
    /// Document root as written (any separators).
    pub document_root: String,
    /// A TLS listener (`*:443`, `SSLEngine on`, `listen ... ssl`) is configured.
    pub ssl: bool,
    /// Wildcard aliases (`*.app.test`) that were left out.
    pub wildcards: Vec<String>,
}

/// Result of reading one file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedConf {
    pub vhosts: Vec<Vhost>,
    /// Why some blocks gave no virtual host (no name, no root, unresolved variable).
    pub ignored: Vec<String>,
}

// -------------------------------------------------------------------- shared

fn is_wildcard(name: &str) -> bool {
    name.contains('*') || name.contains('$') || name.starts_with('~')
}

/// Splits `text` into words; double or single quotes group a word (`"C:/my dir"`).
fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut in_word = false;
    for c in text.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => current.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                in_word = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    out.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            (None, c) => {
                current.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        out.push(current);
    }
    out
}

fn merge(vhosts: &mut Vec<Vhost>, new: Vhost) {
    let key = new.server_name.to_ascii_lowercase();
    match vhosts
        .iter_mut()
        .find(|v| v.server_name.to_ascii_lowercase() == key)
    {
        Some(existing) => {
            existing.ssl |= new.ssl;
            for alias in new.aliases {
                if !existing.aliases.contains(&alias) {
                    existing.aliases.push(alias);
                }
            }
            for wildcard in new.wildcards {
                if !existing.wildcards.contains(&wildcard) {
                    existing.wildcards.push(wildcard);
                }
            }
        }
        None => vhosts.push(new),
    }
}

// -------------------------------------------------------------------- apache

#[derive(Default)]
struct ApacheBlock {
    port: Option<u16>,
    server_name: Option<String>,
    aliases: Vec<String>,
    root: Option<String>,
    ssl_engine: bool,
}

/// Replaces `${NAME}` with the value defined by `Define`; unknown names are left as they
/// are (the caller detects the leftover `${`).
fn expand(value: &str, defines: &BTreeMap<String, String>) -> String {
    let mut text = value.to_owned();
    // Defines may refer to other defines; a few rounds settle any sane file and a cycle
    // cannot loop forever.
    for _ in 0..8 {
        let Some(start) = text.find("${") else { break };
        let Some(len) = text[start..].find('}') else {
            break;
        };
        let name = text[start + 2..start + len].to_ascii_lowercase();
        match defines.get(&name) {
            Some(replacement) => text.replace_range(start..=start + len, replacement),
            None => break,
        }
    }
    text
}

fn apache_port(header: &str) -> Option<u16> {
    // `<VirtualHost *:443>`, `<VirtualHost 127.0.0.1:80 [::1]:80>`
    header
        .trim_start_matches('<')
        .trim_end_matches('>')
        .split_whitespace()
        .skip(1)
        .find_map(|addr| addr.rsplit_once(':').and_then(|(_, p)| p.parse().ok()))
}

/// Reads an Apache file (see the module docs).
pub fn parse_apache(text: &str) -> ParsedConf {
    let mut defines: BTreeMap<String, String> = BTreeMap::new();
    let mut parsed = ParsedConf::default();
    let mut block: Option<ApacheBlock> = None;
    for raw in text.trim_start_matches('\u{feff}').lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("<virtualhost") {
            block = Some(ApacheBlock {
                port: apache_port(line),
                ..ApacheBlock::default()
            });
            continue;
        }
        if lower.starts_with("</virtualhost") {
            if let Some(done) = block.take() {
                finish_apache(done, &mut parsed);
            }
            continue;
        }
        let (name, rest) = line
            .split_once(char::is_whitespace)
            .map_or((line, ""), |(n, r)| (n, r.trim()));
        let name = name.to_ascii_lowercase();
        match (name.as_str(), block.as_mut()) {
            ("define", _) => {
                let args = words(rest);
                if let [key, value, ..] = args.as_slice() {
                    defines.insert(key.to_ascii_lowercase(), expand(value, &defines));
                }
            }
            ("servername", Some(b)) => {
                b.server_name = words(&expand(rest, &defines)).into_iter().next();
            }
            ("serveralias", Some(b)) => {
                b.aliases.extend(words(&expand(rest, &defines)));
            }
            ("documentroot", Some(b)) => {
                b.root = words(&expand(rest, &defines)).into_iter().next();
            }
            ("sslengine", Some(b)) => {
                b.ssl_engine = rest.eq_ignore_ascii_case("on");
            }
            _ => {}
        }
    }
    parsed
}

fn finish_apache(block: ApacheBlock, parsed: &mut ParsedConf) {
    let Some(name) = block.server_name.filter(|n| !n.is_empty()) else {
        parsed.ignored.push("VirtualHost sem ServerName".to_owned());
        return;
    };
    if name.contains("${") {
        parsed
            .ignored
            .push(format!("ServerName com variável não resolvida: {name}"));
        return;
    }
    let Some(root) = block.root.filter(|r| !r.is_empty() && !r.contains("${")) else {
        parsed
            .ignored
            .push(format!("{name}: VirtualHost sem DocumentRoot utilizável"));
        return;
    };
    let (wildcards, aliases): (Vec<String>, Vec<String>) =
        block.aliases.into_iter().partition(|a| is_wildcard(a));
    merge(
        &mut parsed.vhosts,
        Vhost {
            server_name: name,
            aliases,
            document_root: root,
            ssl: block.port == Some(443) || block.ssl_engine,
            wildcards,
        },
    );
}

// --------------------------------------------------------------------- nginx

#[derive(Debug, PartialEq, Eq)]
enum Token {
    Word(String),
    Open,
    Close,
    Semi,
}

fn tokenize(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut chars = text.trim_start_matches('\u{feff}').chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            '#' => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '{' => {
                out.push(Token::Open);
                chars.next();
            }
            '}' => {
                out.push(Token::Close);
                chars.next();
            }
            ';' => {
                out.push(Token::Semi);
                chars.next();
            }
            c if c.is_whitespace() => {
                chars.next();
            }
            '"' | '\'' => {
                let quote = c;
                chars.next();
                let mut word = String::new();
                for c in chars.by_ref() {
                    if c == quote {
                        break;
                    }
                    word.push(c);
                }
                out.push(Token::Word(word));
            }
            _ => {
                let mut word = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() || matches!(c, '{' | '}' | ';' | '#') {
                        break;
                    }
                    word.push(c);
                    chars.next();
                }
                out.push(Token::Word(word));
            }
        }
    }
    out
}

#[derive(Debug)]
struct Directive {
    name: String,
    args: Vec<String>,
    block: Option<Vec<Directive>>,
}

/// Parses directives until the matching `}` (or the end of input).
fn parse_directives(tokens: &[Token], pos: &mut usize) -> Vec<Directive> {
    let mut out = Vec::new();
    let mut current: Vec<String> = Vec::new();
    while *pos < tokens.len() {
        let token = &tokens[*pos];
        *pos += 1;
        match token {
            Token::Word(w) => current.push(w.clone()),
            Token::Semi => {
                if !current.is_empty() {
                    let mut it = std::mem::take(&mut current).into_iter();
                    out.push(Directive {
                        name: it.next().unwrap_or_default(),
                        args: it.collect(),
                        block: None,
                    });
                }
            }
            Token::Open => {
                let block = parse_directives(tokens, pos);
                let mut it = std::mem::take(&mut current).into_iter();
                out.push(Directive {
                    name: it.next().unwrap_or_default(),
                    args: it.collect(),
                    block: Some(block),
                });
            }
            Token::Close => return out,
        }
    }
    out
}

fn collect_servers<'a>(directives: &'a [Directive], out: &mut Vec<&'a Directive>) {
    for d in directives {
        match (&d.block, d.name.as_str()) {
            (Some(_), "server") => out.push(d),
            (Some(inner), _) => collect_servers(inner, out),
            _ => {}
        }
    }
}

/// Reads an Nginx file: every `server { ... }` with a `server_name` and a `root` at the
/// server level (a `root` inside `location` is not the site's root).
pub fn parse_nginx(text: &str) -> ParsedConf {
    let tokens = tokenize(text);
    let mut pos = 0;
    let tree = parse_directives(&tokens, &mut pos);
    let mut servers = Vec::new();
    collect_servers(&tree, &mut servers);
    let mut parsed = ParsedConf::default();
    for server in servers {
        let body = server.block.as_deref().unwrap_or(&[]);
        let mut names: Vec<String> = Vec::new();
        let mut wildcards = Vec::new();
        let mut root = None;
        let mut ssl = false;
        for d in body {
            match d.name.as_str() {
                "server_name" => {
                    for name in &d.args {
                        if name == "_" || name.is_empty() {
                            continue;
                        }
                        if is_wildcard(name) {
                            wildcards.push(name.clone());
                        } else {
                            names.push(name.clone());
                        }
                    }
                }
                "root" if root.is_none() => root = d.args.first().cloned(),
                "listen" => {
                    ssl |= d
                        .args
                        .iter()
                        .any(|a| a == "ssl" || a == "443" || a.ends_with(":443"));
                }
                _ => {}
            }
        }
        if names.is_empty() {
            parsed
                .ignored
                .push("server sem server_name utilizável".to_owned());
            continue;
        }
        let name = names.remove(0);
        let Some(root) = root.filter(|r| !r.is_empty() && !r.contains('$')) else {
            parsed
                .ignored
                .push(format!("{name}: server sem root utilizável"));
            continue;
        };
        merge(
            &mut parsed.vhosts,
            Vhost {
                server_name: name,
                aliases: names,
                document_root: root,
                ssl,
                wildcards,
            },
        );
    }
    parsed
}

#[cfg(test)]
mod tests {
    use super::*;

    const AUTO: &str = r#"define ROOT "C:/laragon/www/Processa"
define SITE "Processa.test"

<VirtualHost *:80>
    DocumentRoot "${ROOT}"
    ServerName ${SITE}
    ServerAlias *.${SITE}
    <Directory "${ROOT}">
        AllowOverride All
        Require all granted
    </Directory>
</VirtualHost>

<VirtualHost *:443>
    DocumentRoot "${ROOT}"
    ServerName ${SITE}
    ServerAlias *.${SITE} www.${SITE}
    SSLEngine on
    SSLCertificateFile      C:/laragon/etc/ssl/laragon.crt
</VirtualHost>
"#;

    #[test]
    fn laragon_auto_vhost_is_expanded_and_merged() {
        let parsed = parse_apache(AUTO);
        assert!(parsed.ignored.is_empty(), "{:?}", parsed.ignored);
        assert_eq!(
            parsed.vhosts,
            [Vhost {
                server_name: "Processa.test".into(),
                aliases: vec!["www.Processa.test".into()],
                document_root: "C:/laragon/www/Processa".into(),
                ssl: true,
                wildcards: vec!["*.Processa.test".into()],
            }]
        );
    }

    #[test]
    fn http_only_vhost_is_not_secure() {
        let parsed = parse_apache(
            "Define ROOT C:/www/a\nDefine SITE a.test\n<VirtualHost *:80>\nDocumentRoot ${ROOT}\nServerName ${SITE}\n</VirtualHost>",
        );
        assert_eq!(parsed.vhosts.len(), 1);
        assert!(!parsed.vhosts[0].ssl);
        assert_eq!(parsed.vhosts[0].document_root, "C:/www/a");
    }

    #[test]
    fn broken_blocks_are_reported_not_fatal() {
        let parsed = parse_apache(
            "<VirtualHost *:80>\nServerName x.test\n</VirtualHost>\n\
             <VirtualHost *:80>\nDocumentRoot C:/a\n</VirtualHost>\n\
             <VirtualHost *:80>\nServerName ${NOPE}\nDocumentRoot C:/a\n</VirtualHost>\n\
             <VirtualHost *:80>\nServerName ok.test\nDocumentRoot \"C:/my dir\"\n</VirtualHost>",
        );
        assert_eq!(parsed.vhosts.len(), 1);
        assert_eq!(parsed.vhosts[0].document_root, "C:/my dir");
        assert_eq!(parsed.ignored.len(), 3);
    }

    #[test]
    fn proxy_style_files_have_no_vhost() {
        let parsed = parse_apache(
            "Listen 8070\n<VirtualHost *:8070>\nProxyPass \"/\" \"https://simpros.test/\"\n</VirtualHost>",
        );
        assert!(parsed.vhosts.is_empty());
        assert_eq!(parsed.ignored.len(), 1);
    }

    #[test]
    fn nginx_servers_are_read_at_server_level_only() {
        let parsed = parse_nginx(
            r#"
# Laragon style
http {
  server {
    listen 80;
    listen 443 ssl;
    server_name  Shop.test  *.shop.test  api.shop.test;
    root "C:/laragon/www/shop/public";
    location / { root /elsewhere; try_files $uri /index.php?$query_string; }
    location ~ \.php$ { fastcgi_pass php_upstream; }
  }
  server { listen 80; server_name _; root C:/x; }
  server { server_name nothing.test; }
}
"#,
        );
        assert_eq!(
            parsed.vhosts,
            [Vhost {
                server_name: "Shop.test".into(),
                aliases: vec!["api.shop.test".into()],
                document_root: "C:/laragon/www/shop/public".into(),
                ssl: true,
                wildcards: vec!["*.shop.test".into()],
            }]
        );
        assert_eq!(parsed.ignored.len(), 2);
    }

    #[test]
    fn unbalanced_input_does_not_panic() {
        assert!(
            parse_nginx("server { server_name a.test;")
                .vhosts
                .is_empty()
        );
        assert!(parse_nginx("}}}{{{;;;").vhosts.is_empty());
        assert!(parse_apache("<VirtualHost").vhosts.is_empty());
    }
}
