//! Purchasable MHO900 software options, queried with `:SYSTem:OPTion:VALid? <name>`.

pub const OPTIONS: &[(&str, &str)] = &[
    ("BWU03T05", "Bandwidth upgrade 350 MHz → 500 MHz"),
    ("BWU03T08", "Bandwidth upgrade 350 MHz → 800 MHz"),
    ("BWU05T08", "Bandwidth upgrade 500 MHz → 800 MHz"),
    ("RLU05", "Memory depth upgrade to 500 Mpts"),
    ("AFG50", "Built-in 2-channel function generator, 50 MHz"),
    ("AFG100", "Built-in 2-channel function generator, 100 MHz"),
    ("AUTOA", "CAN-FD trigger and decode"),
    ("FlexA", "FlexRay trigger and decode"),
    ("AUDIOA", "I2S trigger and decode"),
    ("AEROA", "MIL-STD-1553 trigger and decode"),
];
