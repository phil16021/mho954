//! MHO900 software options (programming guide 3.24.16 – 3.24.19).
//!
//! `:SYSTem:OPTion:STATus?` expects its own tokens (`AUDio`, `CAN-FD`, ...), which differ from
//! the order codes printed on licenses (`AUDIOA`, `AUTOA`, ...). Unknown tokens get no answer.

pub struct OptionDef {
    /// Token for `:SYSTem:OPTion:STATus? <token>`.
    pub token: &'static str,
    /// Order code as used in license strings (`MHO900-<code>@...`).
    pub code: &'static str,
    pub description: &'static str,
}

pub const OPTIONS: &[OptionDef] = &[
    OptionDef {
        token: "BWU03T05",
        code: "BWU03T05",
        description: "Bandwidth upgrade 350 MHz → 500 MHz",
    },
    OptionDef {
        token: "BWU03T08",
        code: "BWU03T08",
        description: "Bandwidth upgrade 350 MHz → 800 MHz",
    },
    OptionDef {
        token: "BWU05T08",
        code: "BWU05T08",
        description: "Bandwidth upgrade 500 MHz → 800 MHz",
    },
    OptionDef {
        token: "RLU-05",
        code: "RLU05",
        description: "Memory depth upgrade to 500 Mpts",
    },
    OptionDef {
        token: "AFG50",
        code: "AFG50",
        description: "2-channel 50 MHz function generator, Bode plot",
    },
    OptionDef {
        token: "AFG100",
        code: "AFG100",
        description: "2-channel 100 MHz function generator, Bode plot",
    },
    OptionDef {
        token: "CAN-FD",
        code: "AUTOA",
        description: "CAN-FD trigger and decode",
    },
    OptionDef {
        token: "FLEX",
        code: "FlexA",
        description: "FlexRay trigger and decode",
    },
    OptionDef {
        token: "AUDio",
        code: "AUDIOA",
        description: "I2S trigger and decode",
    },
    OptionDef {
        token: "AERO",
        code: "AEROA",
        description: "MIL-STD-1553 trigger and decode",
    },
    OptionDef {
        token: "BND",
        code: "BND",
        description: "Bundle: AFG100, AUDIOA, AUTOA, FlexA, AEROA",
    },
];
