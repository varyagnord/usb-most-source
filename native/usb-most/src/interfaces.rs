//! USB interface handoff without Android's implicit kernel-driver reconnect.
pub(crate) trait Control {
    fn release(&mut self, interface: u32) -> Result<(), i32>;
    fn reconnect(&mut self, interface: u32) -> Result<(), i32>;
}

pub(crate) fn release(
    control: &mut impl Control,
    interface: i32,
    reconnect: bool,
) -> Result<(), i32> {
    let interface = u8::try_from(interface).map_err(|_| libc::EINVAL)? as u32;
    let released = control.release(interface);
    if !reconnect {
        return released;
    }
    // A failed configuration change may already have released this interface.
    // Still restore its Android driver when returning the device to the tablet.
    let restored = control.reconnect(interface);
    match released {
        Ok(()) | Err(libc::EINVAL) => restored,
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Hid {
        claimed: bool,
        driver: bool,
    }
    impl Control for Hid {
        fn release(&mut self, _: u32) -> Result<(), i32> {
            if !std::mem::replace(&mut self.claimed, false) {
                return Err(libc::EINVAL);
            }
            Ok(())
        }
        fn reconnect(&mut self, _: u32) -> Result<(), i32> {
            if self.claimed {
                return Err(libc::EBUSY);
            }
            self.driver = true;
            Ok(())
        }
    }
    impl Hid {
        fn set_configuration(&self) -> Result<(), i32> {
            if self.claimed || self.driver {
                Err(libc::EBUSY)
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn hid_reconfiguration_does_not_rebind_android_driver() {
        let mut old = Hid {
            claimed: true,
            driver: false,
        };
        release(&mut old, 1, true).unwrap(); // Android releaseInterface behavior.
        assert_eq!(old.set_configuration(), Err(libc::EBUSY));
        let mut fixed = Hid {
            claimed: true,
            driver: false,
        };
        release(&mut fixed, 1, false).unwrap();
        assert_eq!(fixed.set_configuration(), Ok(()));
        fixed.claimed = true;
        release(&mut fixed, 1, true).unwrap();
        assert!(fixed.driver); // Only session teardown returns the driver.
    }

    #[test]
    fn failed_reconfiguration_still_returns_driver() {
        let mut hid = Hid {
            claimed: true,
            driver: false,
        };
        release(&mut hid, 2, false).unwrap();
        assert_eq!(release(&mut hid, 2, false), Err(libc::EINVAL));
        release(&mut hid, 2, true).unwrap();
        assert!(hid.driver);
    }

    #[test]
    fn invalid_interface_never_changes_ownership() {
        for interface in [-1, 256, i32::MIN, i32::MAX] {
            let mut hid = Hid {
                claimed: true,
                driver: false,
            };
            assert_eq!(release(&mut hid, interface, true), Err(libc::EINVAL));
            assert!(hid.claimed && !hid.driver);
        }
    }
}
