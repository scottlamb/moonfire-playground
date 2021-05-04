use bytes::Bytes;
use failure::{Error, bail};
use log::{debug, info};
use rtcp::packet::Packet;

pub struct TimestampPrinter {
    prev_sr: Option<rtcp::sender_report::SenderReport>,
}

impl TimestampPrinter {
    pub fn new() -> Self {
        TimestampPrinter {
            prev_sr: None,
        }
    }
}

impl super::ChannelHandler for TimestampPrinter {
    fn data(&mut self, rtsp_ctx: crate::Context, timeline: &mut crate::Timeline, mut data: Bytes) -> Result<(), Error> {
        while !data.is_empty() {
            let h = match rtcp::header::Header::unmarshal(&data) {
                Err(e) => bail!("corrupt RTCP header at {:#?}: {}", &rtsp_ctx, e),
                Ok(h) => h,
            };
            let pkt_len = (usize::from(h.length) + 1) * 4;
            if pkt_len > data.len() {
                bail!("rtcp pkt len {} vs remaining body len {} at {:#?}", pkt_len, data.len(), &rtsp_ctx);
            }
            let pkt = data.split_to(pkt_len);
            if h.packet_type == rtcp::header::PacketType::SenderReport {
                let pkt = match rtcp::sender_report::SenderReport::unmarshal(&pkt) {
                    Err(e) => bail!("corrupt RTCP SR at {:#?}: {}", &rtsp_ctx, e),
                    Ok(p) => p,
                };

                // TODO: this should share a 64-bit RTP timestamp with rtp::StrictSequenceNumber.
                let timestamp = match timeline.advance(pkt.rtp_time) {
                    Ok(ts) => ts,
                    Err(e) => return Err(e.context(format!("bad RTP timestamp in RTCP SR {:#?} at {:#?}", &pkt, &rtsp_ctx)).into()),
                };
                info!("rtcp sender report, ts={} ntp={:?}", timestamp, crate::NtpTimestamp(pkt.ntp_time));
                self.prev_sr = Some(pkt);
            } else if h.packet_type == rtcp::header::PacketType::SourceDescription {
                let _pkt = rtcp::source_description::SourceDescription::unmarshal(&pkt)?;
                //println!("rtcp source description: {:#?}", &pkt);
            } else {
                debug!("rtcp: {:?}", h.packet_type);
            }
        }
        Ok(())
    }

    fn end(&mut self) -> Result<(), Error> {
        Ok(())
    }
}
