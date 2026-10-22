//! Единая канонизация адресов абонентов (`sip:`, `h323:`, `rtsp:` и т.п.).
//!
//! T10: одна и та же функция используется API-справочником и дедупликацией
//! `pnm-definitions-v1`. Политика консервативная:
//! - схема приводится к нижнему регистру, `auto:` трактуется как `sip:`;
//! - у host приводится к нижнему регистру только сам host, userinfo,
//!   параметры (`;...`) и query (`?...`) сохраняют регистр;
//! - default port убирается только явно указанный: `5060` для `sip:` и `1720`
//!   для `h323:`; порт у IPv6 в скобках не ломается;
//! - если схема не распознана, сохраняется исторический fallback `sip:<raw>`
//!   (валидацию протокола выполняет вызывающий код).
//!
//! Функция идемпотентна для URI с валидной схемой: `canonical_uri(canonical_uri(x)) == canonical_uri(x)`.

/// Канонизирует адрес абонента для единого справочника и дедупликации.
#[must_use]
pub fn canonical_uri(raw: &str) -> String {
    let trimmed = raw.trim();
    let Some((scheme, rest)) = trimmed.split_once(':') else {
        // Scheme-less форма (`Example.COM`, `192.168.1.132:5060`) обязана
        // проходить ту же политику, иначе canonical(canonical(x)) != x и
        // повторный POST создаёт дубль.
        return format!("sip:{}", canonicalize_host_path(trimmed, "sip"));
    };
    if !valid_scheme(scheme) || scheme.contains('.') || scheme.eq_ignore_ascii_case("localhost") {
        // `Example.COM:5060` / `localhost:5060` — это host:port, а не схема:
        // точки в scheme валидны формально, но в адресах абонентов означают
        // host. Иначе API отклонит protocol `example.com`.
        return format!("sip:{}", canonicalize_host_path(trimmed, "sip"));
    }

    let scheme = if scheme.eq_ignore_ascii_case("auto") { "sip".to_string() } else { scheme.to_ascii_lowercase() };

    if rest.is_empty() {
        return format!("{scheme}:");
    }
    if let Some(authority) = rest.strip_prefix("//") {
        return format!("{scheme}:{}", canonicalize_authority(authority, &scheme));
    }
    if !host_bearing_scheme(&scheme) {
        return format!("{scheme}:{rest}");
    }
    format!("{scheme}:{}", canonicalize_host_path(rest, &scheme))
}

fn valid_scheme(scheme: &str) -> bool {
    let mut characters = scheme.chars();
    characters.next().is_some_and(|character| character.is_ascii_alphabetic())
        && characters.all(|character| character.is_ascii_alphanumeric() || "+.-".contains(character))
}

/// Схемы, у которых даже без `//` часть после `:` начинается с host (возможно,
/// с userinfo). Для остальных opaque-часть не трогаем, чтобы не принять,
/// например, `mailto:User@Example.COM` за host.
fn host_bearing_scheme(scheme: &str) -> bool {
    matches!(scheme, "sip" | "sips" | "h323" | "rtsp" | "vnc" | "media" | "http" | "https" | "ws" | "wss" | "ftp")
}

/// `authority` — часть после `//` до первого `/`, `;`, `?` или `#`.
fn canonicalize_authority(authority: &str, scheme: &str) -> String {
    let split = authority.find(['/', ';', '?', '#']).unwrap_or(authority.len());
    let (head, suffix) = authority.split_at(split);
    format!("//{}{}", normalize_userinfo_host_port(head, scheme), suffix)
}

/// Разбирает `[userinfo@]host[:port][/path][;params][?query][#fragment]` без `//`.
fn canonicalize_host_path(rest: &str, scheme: &str) -> String {
    let split = rest.find(['/', ';', '?', '#']).unwrap_or(rest.len());
    let (head, suffix) = rest.split_at(split);
    format!("{}{}", normalize_userinfo_host_port(head, scheme), suffix)
}

fn normalize_userinfo_host_port(value: &str, scheme: &str) -> String {
    let (userinfo, host_port) = match value.rfind('@') {
        Some(index) => (&value[..=index], &value[index + 1..]),
        None => ("", value),
    };
    format!("{userinfo}{}", normalize_host_port(host_port, scheme))
}

fn normalize_host_port(host_port: &str, scheme: &str) -> String {
    if let Some(after_open) = host_port.strip_prefix('[') {
        let Some(end) = after_open.find(']') else {
            return host_port.to_string();
        };
        let host = after_open[..end].to_ascii_lowercase();
        let after = &after_open[end + 1..];
        let bracketed = format!("[{host}]");
        let Some(port) = after.strip_prefix(':') else {
            // SIP UAC собирает SocketAddr и для IPv6 требует порт: неявный
            // default материализуем, чтобы implicit/explicit формы совпали.
            return if scheme == "sip" { format!("{bracketed}:5060{after}") } else { format!("{bracketed}{after}") };
        };
        if port.is_empty() || !port.chars().all(|character| character.is_ascii_digit()) {
            return format!("{bracketed}:{port}");
        }
        // У bracketed IPv6 явный порт не убираем: SIP UAC пока IPv4-only и
        // без порта такой URI несобираем; сохраняем вход как рабочий.
        format!("{bracketed}:{port}")
    } else {
        let Some((host, port)) = host_port.rsplit_once(':') else {
            return host_port.to_ascii_lowercase();
        };
        // Незакрытый/невалидный IPv6 или нечисловой порт не переписываем:
        // лучше сохранить вход, чем испортить адрес.
        if host.contains(':') || port.is_empty() || !port.chars().all(|character| character.is_ascii_digit()) {
            return host_port.to_string();
        }
        let host = host.to_ascii_lowercase();
        if is_default_port(scheme, port) {
            host
        } else {
            format!("{host}:{port}")
        }
    }
}

fn is_default_port(scheme: &str, port: &str) -> bool {
    (scheme == "sip" && port == "5060") || (scheme == "h323" && port == "1720")
}

#[cfg(test)]
mod tests {
    use super::canonical_uri;

    #[test]
    fn lowercases_scheme_and_host_but_preserves_userinfo() {
        assert_eq!(canonical_uri("SIP:Alice@Example.COM:5060"), "sip:Alice@example.com");
        assert_eq!(canonical_uri("  h323:Host:1720  "), "h323:host");
        assert_eq!(canonical_uri("AUTO:Alice@Example.COM:5060"), "sip:Alice@example.com");
        assert_eq!(canonical_uri("sip:Alice@Example.COM:5061"), "sip:Alice@example.com:5061");
        assert_eq!(canonical_uri("h323:Host:1721"), "h323:host:1721");
        assert_eq!(canonical_uri("sip:Alice@Host:1720"), "sip:Alice@host:1720");
    }

    #[test]
    fn preserves_params_query_and_path_case() {
        assert_eq!(
            canonical_uri("SIP:Alice@Example.COM:5060;transport=TCP?Subject=Hi"),
            "sip:Alice@example.com;transport=TCP?Subject=Hi"
        );
        assert_eq!(
            canonical_uri("sip:Alice@Host;transport=TCP?Subject=Hi#Fragment"),
            "sip:Alice@host;transport=TCP?Subject=Hi#Fragment"
        );
        assert_eq!(
            canonical_uri("rtsp://User:Pass@Example.COM:554/Path?Q=Case"),
            "rtsp://User:Pass@example.com:554/Path?Q=Case"
        );
        assert_eq!(canonical_uri("sip://Host:5060/Path"), "sip://host/Path");
        assert_eq!(canonical_uri("sip:Alice@Host:5060/Path"), "sip:Alice@host/Path");
        assert_eq!(canonical_uri("rtsp:Example.COM:554/Stream"), "rtsp:example.com:554/Stream");
    }

    #[test]
    fn keeps_bracketed_ipv6_and_only_removes_known_default_ports() {
        assert_eq!(canonical_uri("sip:Alice@[2001:DB8::1]:5060"), "sip:Alice@[2001:db8::1]:5060");
        assert_eq!(canonical_uri("sip:Alice@[2001:DB8::1]:5061"), "sip:Alice@[2001:db8::1]:5061");
        assert_eq!(canonical_uri("sip:Alice@[2001:DB8::1]"), "sip:Alice@[2001:db8::1]:5060");
        assert_eq!(canonical_uri("sip://[2001:DB8::1]:5060/Path"), "sip://[2001:db8::1]:5060/Path");
        assert_eq!(canonical_uri("rtsp://Host:5060/Path"), "rtsp://host:5060/Path");
        assert_eq!(canonical_uri("sip:Host:1720"), "sip:host:1720");
    }

    #[test]
    fn fallback_and_valid_scheme_are_compatible_with_old_contract() {
        assert_eq!(canonical_uri("192.168.1.132:5060"), "sip:192.168.1.132");
        assert_eq!(canonical_uri("Example.COM"), "sip:example.com");
        assert_eq!(canonical_uri("Example.COM:5060"), "sip:example.com");
        assert_eq!(canonical_uri("localhost:5060"), "sip:localhost");
        assert_eq!(canonical_uri("1sip:Host"), "sip:1sip:Host");
        assert_eq!(canonical_uri(""), "sip:");
        assert_eq!(canonical_uri("auto:Host"), "sip:host");
    }

    #[test]
    fn canonicalization_is_idempotent_for_valid_uris() {
        for raw in [
            "SIP:Alice@Example.COM:5060",
            "sip:Alice@Example.COM:5060;transport=TCP?Subject=Hi",
            "h323:Host:1720",
            "auto:Alice@Example.COM:5060",
            "rtsp://User:Pass@Example.COM:554/Path?Q=Case",
            "sip:Alice@[2001:DB8::1]:5060",
            "Example.COM",
            "192.168.1.132:5060",
            "sip:Alice@Example.COM",
            "sip://Host:5060/Path",
        ] {
            let once = canonical_uri(raw);
            assert_eq!(canonical_uri(&once), once, "not idempotent for {raw:?}");
        }
    }
}
