use failure::{Error, bail, format_err};

use crate::Timestamp;

const MAX_FORWARD_TIME_JUMP_SECS: u32 = 10;

/// Creates [Timestamp]s (which don't wrap and can be converted to NPT aka normal play time)
/// from 32-bit (wrapping) RTP timestamps.
#[derive(Debug)]
pub(super) struct Timeline {
    timestamp: u64,
    clock_rate: u32,
    start: Option<u32>,
    max_forward_jump: u32,
}

impl Timeline {
    /// Creates a new timeline, erroring on crazy clock rates.
    pub(super) fn new(start: Option<u32>, clock_rate: u32) -> Result<Self, Error> {
        if clock_rate == 0 {
            bail!("clock_rate=0 rejected to prevent division by zero");
        }
        let max_forward_jump = MAX_FORWARD_TIME_JUMP_SECS
            .checked_mul(clock_rate)
            .ok_or_else(|| format_err!(
                "clock_rate={} rejected because max forward jump of {} sec exceeds u32::MAX",
                clock_rate, MAX_FORWARD_TIME_JUMP_SECS))?;
        Ok(Timeline {
            timestamp: u64::from(start.unwrap_or(0)),
            start,
            clock_rate,
            max_forward_jump,
        })
    }

    /// Advances to the given (wrapping) RTP timestamp, creating a monotonically
    /// increasing [Timestamp]. Errors on excessive or backward time jumps.
    pub(super) fn advance_to(&mut self, rtp_timestamp: u32) -> Result<Timestamp, Error> {
        let start = match self.start {
            None => {
                self.start = Some(rtp_timestamp);
                self.timestamp = u64::from(rtp_timestamp);
                rtp_timestamp
            },
            Some(start) => start,
        };
        let forward_delta = rtp_timestamp.wrapping_sub(self.timestamp as u32);
        let forward_ts = Timestamp {
            timestamp: self.timestamp.checked_add(u64::from(forward_delta)).ok_or_else(|| {
                // This probably won't happen even with a hostile server. It'd
                // take (2^32 - 1) packets (~ 4 billion) to advance the time
                // this far, even with a clock rate chosen to maximize
                // max_forward_jump for our MAX_FORWARD_TIME_JUMP_SECS.
                format_err!("timestamp {} + {} will exceed u64::MAX!",
                            self.timestamp, forward_delta)
            })?,
            clock_rate: self.clock_rate,
            start,
        };
        if forward_delta > self.max_forward_jump {
            let f64_clock_rate = f64::from(self.clock_rate);
            let backward_delta = (self.timestamp as u32).wrapping_sub(rtp_timestamp);
            bail!("Timestamp jumped:\n\
                  * forward by  {:10} ({:10.03} sec) from {} to {}, more than allowed {} sec OR\n\
                  * backward by {:10} ({:10.03} sec), more than allowed 0 sec",
                  forward_delta, (forward_delta as f64) / f64_clock_rate, self.timestamp,
                  forward_ts, MAX_FORWARD_TIME_JUMP_SECS, backward_delta,
                  (backward_delta as f64) / f64_clock_rate);
        }
        self.timestamp = forward_ts.timestamp;
        Ok(forward_ts)
    }
}

#[cfg(test)]
mod tests {
    use super::Timeline;

    #[test]
    fn timeline() {
        // Don't allow crazy clock rates that will get us into trouble.
        Timeline::new(Some(0), 0).unwrap_err();
        Timeline::new(Some(0), u32::MAX).unwrap_err();

        // Don't allow excessive forward jumps.
        let mut t = Timeline::new(Some(100), 90_000).unwrap();
        t.advance_to(100 + (super::MAX_FORWARD_TIME_JUMP_SECS * 90_000) + 1).unwrap_err();

        // Or any backward jump.
        let mut t = Timeline::new(Some(100), 90_000).unwrap();
        t.advance_to(99).unwrap_err();

        // Normal usage.
        let mut t = Timeline::new(Some(42), 90_000).unwrap();
        assert_eq!(t.advance_to(83).unwrap().elapsed(), 83 - 42);
        assert_eq!(t.advance_to(453).unwrap().elapsed(), 453 - 42);

        // Wraparound is normal too.
        let mut t = Timeline::new(Some(u32::MAX), 90_000).unwrap();
        assert_eq!(t.advance_to(5).unwrap().elapsed(), 5 + 1);

        // No initial rtptime.
        let mut t = Timeline::new(None, 90_000).unwrap();
        assert_eq!(t.advance_to(218250000).unwrap().elapsed(), 0);
    }
}
