//! Hybrid logical clock for sync versions.
//!
//! A version is `<unix-ms:013>-<counter:06>-<device-id>`; comparing the strings
//! orders them. The wall-clock part keeps versions near real time, the counter
//! orders edits within one millisecond and after the clock goes backwards, and
//! the device id breaks ties the same way on every device. Observing a remote
//! version moves the clock past it, so a device with a slow clock still writes
//! versions newer than everything it has seen.

pub struct Hlc {
    device: String,
    ms: u64,
    counter: u32,
}

fn parse(version: &str) -> Option<(u64, u32)> {
    let mut parts = version.splitn(3, '-');
    let ms = parts.next()?.parse().ok()?;
    let counter = parts.next()?.parse().ok()?;
    Some((ms, counter))
}

impl Hlc {
    /// `last` is the most recent version this device produced or observed.
    pub fn new(device: &str, last: Option<&str>) -> Self {
        let (ms, counter) = last.and_then(parse).unwrap_or((0, 0));
        Self { device: device.to_string(), ms, counter }
    }

    pub fn tick(&mut self, now_ms: u64) -> String {
        if now_ms > self.ms {
            self.ms = now_ms;
            self.counter = 0;
        } else {
            self.counter += 1;
        }
        self.last()
    }

    pub fn observe(&mut self, version: &str) {
        let Some((ms, counter)) = parse(version) else { return };
        if (ms, counter) > (self.ms, self.counter) {
            self.ms = ms;
            self.counter = counter;
        }
    }

    pub fn last(&self) -> String {
        format!("{:013}-{:06}-{}", self.ms, self.counter, self.device)
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_is_monotonic_under_clock_regression() {
        let mut clock = Hlc::new("a", None);
        let first = clock.tick(5_000);
        let same_ms = clock.tick(5_000);
        let earlier = clock.tick(4_000);
        assert!(first < same_ms && same_ms < earlier, "{first} {same_ms} {earlier}");
        assert!(clock.tick(6_000) > earlier);
    }

    #[test]
    fn observe_advances_past_remote() {
        let mut clock = Hlc::new("a", None);
        clock.observe("0000000009000-000004-b");
        let next = clock.tick(1_000);
        assert!(next.as_str() > "0000000009000-000004-b", "{next}");
        // A malformed version is ignored.
        clock.observe("junk");
        assert!(clock.tick(1_000) > next);
    }

    #[test]
    fn versions_compare_as_strings_and_restore() {
        let mut clock = Hlc::new("a", None);
        let small = clock.tick(999);
        let large = clock.tick(10_000);
        assert!(small < large);
        let mut restored = Hlc::new("a", Some(&clock.last()));
        assert!(restored.tick(0) > large);
        // Same instant on two devices: the device id decides.
        assert!(Hlc::new("a", None).tick(7) < Hlc::new("b", None).tick(7));
    }
}
