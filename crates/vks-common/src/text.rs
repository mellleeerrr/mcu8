//! Санитизация пользовательских подписей для UI и раскладок.
//!
//! PNM рисует `display_name` в плитках; emoji/ZWJ/skin-tone, keycap-последовательности
//! и управляющие/bidi-символы ломают центрирование, переносы и часть SIP-заголовков.
//!
//! Политика — **denylist emoji/пиктограмм и format-контролов**, а не allowlist:
//! буквы любого алфавита, combining marks (NFD-кириллица, деванагари, арабика),
//! цифры, типографская пунктуация (`— «» № % & ! ?`) сохраняются. Длина
//! ограничена 128 символами; обрезка не оставляет хвостовой пробел и не рвёт
//! combining-кластер посередине.

/// Максимальная длина подписи в символах.
pub const LABEL_MAX_CHARS: usize = 128;

/// Удаляет emoji, пиктограммы, keycap, ZWJ/VS16/skin-tone/tag-последовательности,
/// управляющие и bidi/format-символы; схлопывает пробелы и обрезает до 128 символов.
pub fn sanitize_label(raw: &str) -> String {
    let mut out: Vec<char> = Vec::with_capacity(raw.chars().count());
    for ch in raw.chars() {
        if ch == '\u{20E3}' {
            // Keycap: `1️⃣` — убираем и базу, иначе останется голая цифра.
            if matches!(out.last(), Some('0'..='9' | '#' | '*')) {
                out.pop();
            }
            continue;
        }
        if is_dropped(ch) {
            continue;
        }
        out.push(ch);
    }
    let collapsed = out.iter().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ");
    let all: Vec<char> = collapsed.chars().collect();
    let cut = all.len() > LABEL_MAX_CHARS;
    let mut truncated: Vec<char> = all.iter().take(LABEL_MAX_CHARS).copied().collect();
    if cut {
        // Не оставляем combining marks без базы и не оставляем базу, у которой
        // mark попал за границу: оба случая — разрыв кластера.
        while truncated.last().is_some_and(|ch| is_combining_mark(*ch)) {
            truncated.pop();
        }
        if all.get(truncated.len()).is_some_and(|ch| is_combining_mark(*ch)) {
            truncated.pop();
        }
    }
    truncated.iter().collect::<String>().trim().to_string()
}

/// Как [`sanitize_label`], но пустой результат заменяется `fallback`.
pub fn sanitize_label_or(raw: &str, fallback: &str) -> String {
    let cleaned = sanitize_label(raw);
    if cleaned.is_empty() {
        fallback.to_string()
    } else {
        cleaned
    }
}

fn is_dropped(ch: char) -> bool {
    let c = ch as u32;
    // Emoji/пиктограммы: BMP-символы и дополнительные плоскости.
    matches!(c,
        0x00A9 | 0x00AE | 0x203C | 0x2049 | 0x2122 | 0x2139 | 0x3030 | 0x303D | 0x3297 | 0x3299
        | 0x1F000..=0x1FAFF // pictographs, emoticons, transport, supplemental
        | 0x2600..=0x27BF // misc symbols + dingbats
        | 0x2B00..=0x2BFF // misc symbols and arrows
        | 0xFE00..=0xFE0F // variation selectors
        | 0xE0020..=0xE007F // tag sequences
        | 0x20E3            // combining enclosing keycap (страховка)
        | 0x200D            // ZWJ
        | 0x200B..=0x200F  // zero-width + bidi marks
        | 0x202A..=0x202E  // bidi embedding/override
        | 0x2060..=0x206F  // word joiner + bidi isolates
        | 0xFEFF           // BOM / zero-width no-break
        | 0x0000..=0x001F  // C0
        | 0x007F..=0x009F  // C1
    ) || ch.is_control()
}

fn is_combining_mark(ch: char) -> bool {
    let c = ch as u32;
    matches!(c,
        0x0300..=0x036F | 0x0483..=0x0489 | 0x0591..=0x05BD
        | 0x05BF | 0x05C1..=0x05C2 | 0x05C4..=0x05C5 | 0x05C7
        | 0x0610..=0x061A | 0x064B..=0x065F | 0x0670
        | 0x06D6..=0x06DC | 0x06DF..=0x06E4 | 0x06E7..=0x06E8 | 0x06EA..=0x06ED
        | 0x0711 | 0x0730..=0x074A | 0x07A6..=0x07B0 | 0x07EB..=0x07F3
        | 0x0816..=0x0819 | 0x081B..=0x0823 | 0x0825..=0x0827 | 0x0829..=0x082D
        | 0x0859..=0x085B | 0x08D3..=0x08E1 | 0x08E3..=0x0903
        | 0x093A..=0x093C | 0x093E..=0x094F | 0x0951..=0x0957 | 0x0962..=0x0963
        | 0x0981..=0x0983 | 0x09BC | 0x09BE..=0x09CD
        | 0x0E31 | 0x0E34..=0x0E3A | 0x0E47..=0x0E4E
        | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_emoji_flags_zwj_skin_tones_and_keycaps() {
        assert_eq!(sanitize_label("Зал 132 🎤"), "Зал 132");
        assert_eq!(sanitize_label("Team 👨‍👩‍👧‍👦 call"), "Team call");
        assert_eq!(sanitize_label("Alex 👍🏽"), "Alex");
        assert_eq!(sanitize_label("flag 🇷🇺"), "flag");
        assert_eq!(sanitize_label("zero\u{200d}width"), "zerowidth");
        assert_eq!(sanitize_label("key 1️⃣ #️⃣ *️⃣"), "key");
        assert_eq!(sanitize_label("tag 🏴\u{e0067}\u{e0062}\u{e007f}"), "tag");
    }

    #[test]
    fn keeps_cyrillic_latin_digits_combining_marks_and_typography() {
        assert_eq!(sanitize_label("  Иван (Москва) — SIP:user_1  "), "Иван (Москва) — SIP:user_1");
        assert_eq!(sanitize_label("Room #5 + 1"), "Room #5 + 1");
        assert_eq!(sanitize_label("«Ёлка» №5, 100% & ok!"), "«Ёлка» №5, 100% & ok!");
        // NFD: `й` = и + U+0306; combining mark не должен теряться.
        let decomposed = "и\u{0306}";
        assert_eq!(sanitize_label(decomposed), decomposed);
    }

    #[test]
    fn is_idempotent_and_bounded_by_chars() {
        let once = sanitize_label("  A 🚀  B  ");
        assert_eq!(sanitize_label(&once), once);
        let long = "Ж".repeat(200);
        assert_eq!(sanitize_label(&long).chars().count(), LABEL_MAX_CHARS);
        // Граница 128: `127 Ж + и + U+0306` (129 символов) не должна оставить
        // `127 Ж + и` — база без своего combining-mark тоже разрыв кластера.
        let boundary = format!("{}и\u{0306}", "Ж".repeat(LABEL_MAX_CHARS - 1));
        assert!(boundary.chars().count() > LABEL_MAX_CHARS);
        let cleaned = sanitize_label(&boundary);
        assert_eq!(cleaned.chars().count(), LABEL_MAX_CHARS - 1, "база с отрезанным mark тоже отбрасывается");
        assert!(!cleaned.contains('и'));
        assert_eq!(sanitize_label(&cleaned), cleaned);
        assert!(!cleaned.ends_with(char::is_whitespace));
        // Кластер целиком помещается — mark сохраняется.
        let fits = format!("{}и\u{0306}", "Ж".repeat(LABEL_MAX_CHARS - 2));
        assert_eq!(sanitize_label(&fits), fits);
    }

    #[test]
    fn fallback_for_empty_or_all_emoji() {
        assert_eq!(sanitize_label_or("🎬🎬", "sip:user"), "sip:user");
        assert_eq!(sanitize_label_or("  ", "fallback"), "fallback");
        assert_eq!(sanitize_label_or("ok", "fallback"), "ok");
    }
}
