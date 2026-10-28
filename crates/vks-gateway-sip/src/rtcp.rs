//! Минимальный RTCP-разбор для видео-плеча шлюза (RFC 3550, RFC 4585).
//!
//! Нужен ровно для одного сценария: терминал просит ключевой кадр через
//! `PLI`/`FIR`, шлюз должен это увидеть и запросить IDR у источника. Полный
//! стек RTCP (SR/RR-статистика, SDES, BYE) не реализуется: SR уже отправляется
//! из `gateway_audio`/`gateway_video`, а приём отчётов пока не нужен.

/// Заголовок RTCP-блока: 2 бита версии, 1 бит padding, 5 бит счётчика.
const MIN_PACKET: usize = 8;
pub const PT_SR: u8 = 200;
pub const PT_RR: u8 = 201;
pub const PT_RTPFB: u8 = 205;
pub const PT_PSFB: u8 = 206;
const FMT_PLI: u8 = 1;
const FMT_FIR: u8 = 4;

/// Найденный в compound-пакете запрос ключевого кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoFeedback {
    pub kind: FeedbackKind,
    /// SSRC отправителя фидбэка (терминал).
    pub sender_ssrc: u32,
    /// SSRC медиа-потока, к которому относится запрос.
    pub media_ssrc: u32,
    /// SSRC, для которого нужен IDR (FCI; 0 если не указан).
    pub target_ssrc: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedbackKind {
    Pli,
    Fir,
}

/// RFC 5761 §4: RTCP от RTP отличают по PT 192..=223 при V=2.
pub fn is_rtcp(packet: &[u8]) -> bool {
    packet.len() >= MIN_PACKET && packet[0] >> 6 == 2 && (192..=223).contains(&packet[1])
}

fn read_u32(packet: &[u8], offset: usize) -> Option<u32> {
    packet.get(offset..offset + 4).map(|bytes| u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Разбирает compound-пакет и возвращает только PLI/FIR.
/// Некорректные блоки пропускаются, но не роняют разбор остальных.
pub fn parse_feedback(packet: &[u8]) -> Vec<VideoFeedback> {
    let mut found = Vec::new();
    let mut offset = 0usize;
    while offset + MIN_PACKET <= packet.len() {
        let first = packet[offset];
        if first >> 6 != 2 {
            break;
        }
        let count = usize::from(first & 0x1f);
        let pt = packet[offset + 1];
        let words = usize::from(u16::from_be_bytes([packet[offset + 2], packet[offset + 3]])) + 1;
        let block_len = words * 4;
        let Some(block) = packet.get(offset..offset + block_len) else { break };
        let mut payload_offset = 4usize;
        let sender_ssrc = read_u32(block, payload_offset).unwrap_or(0);
        payload_offset += 4;
        if pt == PT_SR {
            // sender info (20) + report blocks (24 each) + SDES; только пропуск.
            let _ = count;
        } else if pt == PT_RR {
            let _ = count;
        } else if block.len() >= 8 {
            let media_ssrc = read_u32(block, payload_offset).unwrap_or(0);
            payload_offset += 4;
            if pt == PT_PSFB && block[0] & 0x1f == FMT_PLI {
                let target_ssrc = read_u32(block, payload_offset).unwrap_or(0);
                found.push(VideoFeedback { kind: FeedbackKind::Pli, sender_ssrc, media_ssrc, target_ssrc });
            } else if pt == PT_PSFB && block[0] & 0x1f == FMT_FIR {
                // FIR FCI: SSRC(4) + seq(1) + reserved(3).
                let target_ssrc = read_u32(block, payload_offset).unwrap_or(0);
                found.push(VideoFeedback { kind: FeedbackKind::Fir, sender_ssrc, media_ssrc, target_ssrc });
            }
        }
        offset += block_len;
    }
    found
}

/// Сводка compound-пакета: что в нём было (для счётчиков и метрик).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CompoundSummary {
    pub sr: u64,
    pub rr: u64,
    pub sdes: u64,
    pub bye: u64,
    pub pli: u64,
    pub fir: u64,
}

const PT_SDES: u8 = 202;
const PT_BYE: u8 = 203;

/// Блоки compound-пакета: (PT, FMT/RC, тело блока).
fn blocks(packet: &[u8]) -> Vec<(u8, u8, &[u8])> {
    let mut found = Vec::new();
    let mut offset = 0usize;
    while offset + MIN_PACKET <= packet.len() {
        let first = packet[offset];
        if first >> 6 != 2 {
            break;
        }
        let pt = packet[offset + 1];
        let words = usize::from(u16::from_be_bytes([packet[offset + 2], packet[offset + 3]])) + 1;
        let block_len = words * 4;
        let Some(block) = packet.get(offset..offset + block_len) else { break };
        found.push((pt, first & 0x1f, block));
        offset += block_len;
    }
    found
}

/// Считает блоки compound-пакета (SR/RR/SDES/BYE + PLI/FIR).
pub fn parse_compound(packet: &[u8]) -> CompoundSummary {
    let mut summary = CompoundSummary::default();
    for (pt, fmt, _block) in blocks(packet) {
        match pt {
            PT_SR => summary.sr += 1,
            PT_RR => summary.rr += 1,
            PT_SDES => summary.sdes += 1,
            PT_BYE => summary.bye += 1,
            PT_PSFB if fmt == FMT_PLI => summary.pli += 1,
            PT_PSFB if fmt == FMT_FIR => summary.fir += 1,
            _ => {}
        }
    }
    summary
}

/// Пустой Receiver Report (RFC 3550 §6.4.2): V=2, PT=201, 8 байт.
/// Используется как keepalive раз в 5 с и как ответ на SR.
pub fn build_rr(sender_ssrc: u32) -> [u8; 8] {
    let mut packet = [0u8; 8];
    packet[0] = 0x80; // V=2, P=0, RC=0
    packet[1] = PT_RR;
    packet[2..4].copy_from_slice(&1u16.to_be_bytes());
    packet[4..8].copy_from_slice(&sender_ssrc.to_be_bytes());
    packet
}

/// PLI (RFC 4585 §6.3.1): PSFB, FMT=1, 12 байт.
pub fn build_pli(sender_ssrc: u32, media_ssrc: u32) -> [u8; 12] {
    let mut packet = [0u8; 12];
    packet[0] = 0x81; // V=2, P=0, FMT=1
    packet[1] = PT_PSFB;
    packet[2..4].copy_from_slice(&2u16.to_be_bytes()); // length = 3 words - 1
    packet[4..8].copy_from_slice(&sender_ssrc.to_be_bytes());
    packet[8..12].copy_from_slice(&media_ssrc.to_be_bytes());
    packet
}

/// FIR (RFC 5104 §4.3.1): PSFB, FMT=4, один FCI (SSRC + seq + reserved).
pub fn build_fir(sender_ssrc: u32, media_ssrc: u32, target_ssrc: u32, sequence: u8) -> [u8; 20] {
    let mut packet = [0u8; 20];
    packet[0] = 0x84; // V=2, P=0, FMT=4
    packet[1] = PT_PSFB;
    packet[2..4].copy_from_slice(&4u16.to_be_bytes()); // length = 5 words - 1
    packet[4..8].copy_from_slice(&sender_ssrc.to_be_bytes());
    packet[8..12].copy_from_slice(&media_ssrc.to_be_bytes());
    packet[12..16].copy_from_slice(&target_ssrc.to_be_bytes());
    packet[16] = sequence;
    packet
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pli_is_detected_and_roundtrips() {
        let packet = build_pli(0x1111_2222, 0x3333_4444);
        assert!(is_rtcp(&packet));
        let feedback = parse_feedback(&packet);
        assert_eq!(feedback.len(), 1);
        assert_eq!(feedback[0].kind, FeedbackKind::Pli);
        assert_eq!(feedback[0].sender_ssrc, 0x1111_2222);
        assert_eq!(feedback[0].media_ssrc, 0x3333_4444);
    }

    #[test]
    fn fir_is_detected_with_target_ssrc() {
        let packet = build_fir(1, 2, 0x0a0b_0c0d, 7);
        assert_eq!(
            parse_feedback(&packet),
            vec![VideoFeedback { kind: FeedbackKind::Fir, sender_ssrc: 1, media_ssrc: 2, target_ssrc: 0x0a0b_0c0d }]
        );
    }

    #[test]
    fn receiver_report_is_not_feedback() {
        // RR: V=2, PT=201, length=1 (8 bytes), reporter SSRC.
        let rr = [0x80u8, PT_RR, 0, 1, 0, 0, 0, 1];
        assert!(is_rtcp(&rr));
        assert!(parse_feedback(&rr).is_empty());
    }

    #[test]
    fn compound_packet_keeps_each_feedback_block() {
        let pli = build_pli(10, 20);
        let fir = build_fir(10, 20, 30, 1);
        let mut compound = Vec::new();
        compound.extend_from_slice(&pli);
        compound.extend_from_slice(&fir);
        let feedback = parse_feedback(&compound);
        assert_eq!(feedback.len(), 2);
        assert_eq!(feedback[0].kind, FeedbackKind::Pli);
        assert_eq!(feedback[1].kind, FeedbackKind::Fir);
    }

    #[test]
    fn compound_summary_counts_report_and_feedback_blocks() {
        let rr = build_rr(0x0102_0304);
        let pli = build_pli(1, 2);
        let fir = build_fir(1, 2, 3, 1);
        let mut compound = Vec::new();
        compound.extend_from_slice(&rr);
        compound.extend_from_slice(&pli);
        compound.extend_from_slice(&fir);
        let summary = parse_compound(&compound);
        assert_eq!(summary.rr, 1);
        assert_eq!(summary.pli, 1);
        assert_eq!(summary.fir, 1);
        assert_eq!(summary.sr, 0);
        assert!(is_rtcp(&rr));
    }

    #[test]
    fn pli_is_psfb_fmt1_like_real_terminals_send_it() {
        // Exactly what a Yealink/browser PLI looks like on the wire:
        // V=2, P=0, FMT=1, PT=206(PSFB), length=2, sender SSRC, media SSRC.
        let packet = [0x81u8, 206, 0, 2, 0xde, 0xad, 0xbe, 0xef, 0x00, 0x00, 0x00, 0x2a];
        assert!(is_rtcp(&packet));
        let summary = parse_compound(&packet);
        assert_eq!((summary.pli, summary.fir, summary.sr, summary.rr), (1, 0, 0, 0));
        let feedback = parse_feedback(&packet);
        assert_eq!(feedback.len(), 1);
        assert_eq!(feedback[0].kind, FeedbackKind::Pli);
        assert_eq!(feedback[0].sender_ssrc, 0xdead_beef);
        assert_eq!(feedback[0].media_ssrc, 0x2a);
    }

    #[test]
    fn generic_nack_is_not_a_keyframe_request() {
        // RTPFB (PT 205) FMT=1 is NACK; only PSFB FMT=1 is PLI.
        let nack = [0x81u8, 205, 0, 2, 0, 0, 0, 1, 0, 0, 0, 2];
        let summary = parse_compound(&nack);
        assert_eq!((summary.pli, summary.fir), (0, 0));
        assert!(parse_feedback(&nack).is_empty());
    }

    #[test]
    fn rtp_media_and_truncated_packets_are_rejected() {
        // RTP: PT 96 (audio/video media), not RTCP.
        assert!(!is_rtcp(&[0x80, 96, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]));
        assert!(!is_rtcp(&[0x80, PT_RTPFB, 0]));
        assert!(parse_feedback(&[0x80, PT_RTPFB, 0, 2, 0, 0, 0, 0]).is_empty());
    }
}
