//! Zero-copy SIP 2.0 message parser and serializer for UAC client (RFC 3261).

/// Разобранный SIP-ответ (Zero-alloc срезы из буфера).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SipResponse<'a> {
    pub status_code: u16,
    pub reason: &'a str,
    pub call_id: Option<&'a str>,
    pub cseq_num: u32,
    pub cseq_method: Option<&'a str>,
    pub to_tag: Option<&'a str>,
    pub from_tag: Option<&'a str>,
    pub contact: Option<&'a str>,
    pub body: &'a str,
}

impl<'a> SipResponse<'a> {
    /// Парсит входящий SIP-ответ из текстового буфера.
    pub fn parse(buf: &'a str) -> Option<Self> {
        let (head, body) = if let Some(pos) = buf.find("\r\n\r\n") {
            (&buf[..pos], &buf[pos + 4..])
        } else if let Some(pos) = buf.find("\n\n") {
            (&buf[..pos], &buf[pos + 2..])
        } else {
            (buf, "")
        };

        let mut lines = head.lines();
        let status_line = lines.next()?;
        let mut parts = status_line.splitn(3, ' ');
        let proto = parts.next()?;
        if !proto.starts_with("SIP/2.0") {
            return None;
        }
        let status_code: u16 = parts.next()?.trim().parse().ok()?;
        let reason = parts.next().unwrap_or("").trim();

        let mut resp = SipResponse {
            status_code,
            reason,
            call_id: None,
            cseq_num: 1,
            cseq_method: None,
            to_tag: None,
            from_tag: None,
            contact: None,
            body: body.trim(),
        };

        for line in lines {
            let (name, val) = match line.split_once(':') {
                Some((n, v)) => (n.trim().to_ascii_lowercase(), v.trim()),
                None => continue,
            };
            match name.as_str() {
                "call-id" | "i" => resp.call_id = Some(val),
                "cseq" => {
                    let mut cparts = val.split_whitespace();
                    if let Some(num_str) = cparts.next() {
                        if let Ok(num) = num_str.parse::<u32>() {
                            resp.cseq_num = num;
                        }
                    }
                    resp.cseq_method = cparts.next();
                }
                "contact" | "m" => {
                    let c = val.trim_start_matches('<').split('>').next().unwrap_or(val).trim();
                    resp.contact = Some(c);
                }
                "to" | "t" => {
                    if let Some((_, tag_part)) = val.split_once(";tag=") {
                        let tag = tag_part.split(';').next().unwrap_or(tag_part).trim();
                        resp.to_tag = Some(tag);
                    }
                }
                "from" | "f" => {
                    if let Some((_, tag_part)) = val.split_once(";tag=") {
                        let tag = tag_part.split(';').next().unwrap_or(tag_part).trim();
                        resp.from_tag = Some(tag);
                    }
                }
                _ => {}
            }
        }

        Some(resp)
    }
}

/// Строит RFC 3261 SIP INVITE запрос.
pub fn build_invite(
    target_addr: &str,
    local_ip: &str,
    local_port: u16,
    call_id: &str,
    from_tag: &str,
    branch: &str,
    sdp_body: &str,
) -> String {
    let sdp_len = sdp_body.len();
    format!(
        "INVITE sip:{target_addr} SIP/2.0\r\n\
Via: SIP/2.0/UDP {local_ip}:{local_port};branch=z9hG4bK{branch};rport\r\n\
Max-Forwards: 70\r\n\
From: \"vks-mcu\" <sip:vks@{local_ip}>;tag={from_tag}\r\n\
To: <sip:{target_addr}>\r\n\
Call-ID: {call_id}\r\n\
CSeq: 1 INVITE\r\n\
Contact: <sip:vks@{local_ip}:{local_port};transport=udp>\r\n\
Content-Type: application/sdp\r\n\
Content-Length: {sdp_len}\r\n\
\r\n\
{sdp_body}"
    )
}

/// Строит RFC 3261 SIP ACK подтверждение.
#[allow(clippy::too_many_arguments)]
pub fn build_ack(
    target_addr: &str,
    local_ip: &str,
    local_port: u16,
    call_id: &str,
    from_tag: &str,
    to_tag: Option<&str>,
    branch: &str,
    is_2xx: bool,
) -> String {
    let to_hdr =
        if let Some(tag) = to_tag { format!("<sip:{target_addr}>;tag={tag}") } else { format!("<sip:{target_addr}>") };

    // Для 2xx ACK генерируется новый branch (End-to-End Dialog ACK),
    // для 3xx-6xx используется исходный branch транзакции (Hop-by-Hop Transaction ACK).
    let branch_str =
        if is_2xx { format!("z9hG4bKack-{}", uuid::Uuid::new_v4().simple()) } else { format!("z9hG4bK{branch}") };

    format!(
        "ACK sip:{target_addr} SIP/2.0\r\n\
Via: SIP/2.0/UDP {local_ip}:{local_port};branch={branch_str};rport\r\n\
Max-Forwards: 70\r\n\
From: \"vks-mcu\" <sip:vks@{local_ip}>;tag={from_tag}\r\n\
To: {to_hdr}\r\n\
Call-ID: {call_id}\r\n\
CSeq: 1 ACK\r\n\
Contact: <sip:vks@{local_ip}:{local_port};transport=udp>\r\n\
Content-Length: 0\r\n\
\r\n"
    )
}

/// Строит RFC 3261 SIP BYE запрос для завершения диалога.
pub fn build_bye(
    contact_uri: &str,
    local_ip: &str,
    local_port: u16,
    call_id: &str,
    from_tag: &str,
    to_tag: &str,
    cseq_num: u32,
) -> String {
    let branch = format!("z9hG4bKbye-{}", uuid::Uuid::new_v4().simple());
    let to_hdr = if contact_uri.starts_with('<') {
        let trimmed = contact_uri.trim_end_matches('>');
        format!("{trimmed}>;tag={to_tag}")
    } else if contact_uri.starts_with("sip:") || contact_uri.starts_with("sips:") {
        format!("<{contact_uri}>;tag={to_tag}")
    } else {
        format!("<sip:{contact_uri}>;tag={to_tag}")
    };
    format!(
        "BYE {contact_uri} SIP/2.0\r\n\
Via: SIP/2.0/UDP {local_ip}:{local_port};branch={branch};rport\r\n\
Max-Forwards: 70\r\n\
From: \"vks-mcu\" <sip:vks@{local_ip}>;tag={from_tag}\r\n\
To: {to_hdr}\r\n\
Call-ID: {call_id}\r\n\
CSeq: {cseq_num} BYE\r\n\
Contact: <sip:vks@{local_ip}:{local_port};transport=udp>\r\n\
Content-Length: 0\r\n\
\r\n"
    )
}

/// Строит RFC 3261 SIP CANCEL запрос для отмены вызова до ответа 200 OK.
pub fn build_cancel(
    target_addr: &str,
    local_ip: &str,
    local_port: u16,
    call_id: &str,
    from_tag: &str,
    branch: &str,
) -> String {
    format!(
        "CANCEL sip:{target_addr} SIP/2.0\r\n\
Via: SIP/2.0/UDP {local_ip}:{local_port};branch=z9hG4bK{branch};rport\r\n\
Max-Forwards: 70\r\n\
From: \"vks-mcu\" <sip:vks@{local_ip}>;tag={from_tag}\r\n\
To: <sip:{target_addr}>\r\n\
Call-ID: {call_id}\r\n\
CSeq: 1 CANCEL\r\n\
Contact: <sip:vks@{local_ip}:{local_port};transport=udp>\r\n\
Content-Length: 0\r\n\
\r\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sip_responses() {
        let resp_100 = "SIP/2.0 100 Trying\r\n\
Via: SIP/2.0/UDP 127.0.0.1:5060;branch=z9hG4bK12345\r\n\
From: <sip:vks@127.0.0.1>;tag=f123\r\n\
To: <sip:192.168.1.132>\r\n\
Call-ID: call-abc\r\n\
CSeq: 1 INVITE\r\n\
Content-Length: 0\r\n\r\n";

        let parsed = SipResponse::parse(resp_100).expect("parse 100");
        assert_eq!(parsed.status_code, 100);
        assert_eq!(parsed.reason, "Trying");
        assert_eq!(parsed.call_id, Some("call-abc"));
        assert_eq!(parsed.cseq_method, Some("INVITE"));
        assert_eq!(parsed.cseq_num, 1);
        assert_eq!(parsed.to_tag, None);

        let resp_200 = "SIP/2.0 200 OK\r\n\
Via: SIP/2.0/UDP 127.0.0.1:5060;branch=z9hG4bK12345\r\n\
From: \"vks-mcu\" <sip:vks@127.0.0.1>;tag=f123\r\n\
To: <sip:192.168.1.132>;tag=t987\r\n\
Call-ID: call-abc\r\n\
CSeq: 1 INVITE\r\n\
Contact: <sip:box@192.168.1.132:5060>\r\n\
Content-Type: application/sdp\r\n\
Content-Length: 18\r\n\r\n\
v=0\r\no=term 1 1 IN\r\n";

        let parsed_200 = SipResponse::parse(resp_200).expect("parse 200");
        assert_eq!(parsed_200.status_code, 200);
        assert_eq!(parsed_200.to_tag, Some("t987"));
        assert_eq!(parsed_200.contact, Some("sip:box@192.168.1.132:5060"));
        assert!(parsed_200.body.contains("v=0"));
    }

    #[test]
    fn build_messages_format() {
        let invite = build_invite("192.168.1.132", "192.168.1.93", 5062, "cid-1", "ftag-1", "b-1", "v=0\r\n");
        assert!(invite.starts_with("INVITE sip:192.168.1.132 SIP/2.0\r\n"));
        assert!(invite.contains("Via: SIP/2.0/UDP 192.168.1.93:5062;branch=z9hG4bKb-1;rport\r\n"));
        assert!(invite.contains("CSeq: 1 INVITE\r\n"));
        assert!(invite.contains("Content-Length: 5\r\n"));

        let ack = build_ack("192.168.1.132", "192.168.1.93", 5062, "cid-1", "ftag-1", Some("ttag-1"), "b-1", true);
        assert!(ack.starts_with("ACK sip:192.168.1.132 SIP/2.0\r\n"));
        assert!(ack.contains(";tag=ttag-1"));
        assert!(ack.contains("CSeq: 1 ACK\r\n"));

        let bye = build_bye("sip:box@192.168.1.132:5060", "192.168.1.93", 5062, "cid-1", "ftag-1", "ttag-1", 2);
        assert!(bye.starts_with("BYE sip:box@192.168.1.132:5060 SIP/2.0\r\n"));
        assert!(bye.contains("CSeq: 2 BYE\r\n"));
    }
}
