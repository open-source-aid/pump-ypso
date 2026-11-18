use super::{Versions, YpsoSim, PASSWORD_LEN};
use crate::protocol::KEY_LEN;

impl YpsoSim {
    pub fn opn(password: [u8; PASSWORD_LEN]) -> Self {
        Self {
            versions: Versions {
                pump_base: "1.1".into(),
                master_software: "V65.00.06".into(),
                settings: "1.3".into(),
                history: "1.1".into(),
                control: "1.0".into(),
            },
            crypto_service: false,
            ..Self::new([0; KEY_LEN], password)
        }
    }
}
