//! Tablas constantes del remuestreador de SILK.

/// Orden del FIR de interpolación.
pub(super) const RESAMPLER_ORDER_FIR_12: usize = 8;

/// Coeficientes de los filtros paso todo del duplicador de alta calidad, rama 0.
pub(super) const RESAMPLER_UP2_HQ_0: [i16; 3] = [1746, 14_986, -26_453];

/// Coeficientes de los filtros paso todo del duplicador de alta calidad, rama 1.
pub(super) const RESAMPLER_UP2_HQ_1: [i16; 3] = [6854, 25_769, -9994];

/// Fases fraccionarias del FIR de interpolación: 12 posiciones entre dos muestras de entrada
/// (`silk_resampler_frac_FIR_12`).
pub(super) const RESAMPLER_FIR_FRACTIONS: usize = 12;

/// Primera mitad de los coeficientes del FIR de interpolación de cada fase; la otra mitad es la
/// de la fase simétrica, recorrida al revés.
pub(super) const RESAMPLER_FRAC_FIR_12: [[i16; RESAMPLER_ORDER_FIR_12 / 2];
    RESAMPLER_FIR_FRACTIONS] = [
    [189, -600, 617, 30_567],
    [117, -159, -1070, 29_704],
    [52, 221, -2392, 28_276],
    [-4, 529, -3350, 26_341],
    [-48, 758, -3956, 23_973],
    [-80, 905, -4235, 21_254],
    [-99, 972, -4222, 18_278],
    [-107, 967, -3957, 15_143],
    [-103, 896, -3487, 11_950],
    [-91, 773, -2865, 8798],
    [-71, 611, -2143, 5784],
    [-46, 425, -1375, 2996],
];

/// Orden del FIR de las bajadas 3/4 y 2/3 (`RESAMPLER_DOWN_ORDER_FIR0`).
pub(super) const RESAMPLER_DOWN_ORDER_FIR0: usize = 18;

/// Orden del FIR de la bajada 1/2 (`RESAMPLER_DOWN_ORDER_FIR1`).
pub(super) const RESAMPLER_DOWN_ORDER_FIR1: usize = 24;

/// Bajada 3/4 (de 16 a 12 kHz): los 2 coeficientes del filtro AR de segundo orden, en Q14, y
/// la primera mitad del FIR de cada una de sus 3 fases (`silk_Resampler_3_4_COEFS`).
pub(super) const RESAMPLER_3_4_COEFS: [i16; 2 + 3 * RESAMPLER_DOWN_ORDER_FIR0 / 2] = [
    -20_694, -13_867, //
    -49, 64, 17, -157, 353, -496, 163, 11_047, 22_205, //
    -39, 6, 91, -170, 186, 23, -896, 6336, 19_928, //
    -19, -36, 102, -89, -24, 328, -951, 2568, 15_909,
];

/// Bajada 2/3 (de 12 a 8 kHz), con 2 fases (`silk_Resampler_2_3_COEFS`).
pub(super) const RESAMPLER_2_3_COEFS: [i16; 2 + 2 * RESAMPLER_DOWN_ORDER_FIR0 / 2] = [
    -14_457, -14_019, //
    64, 128, -122, 36, 310, -768, 584, 9267, 17_733, //
    12, 128, 18, -142, 288, -117, -865, 4123, 14_459,
];

/// Bajada 1/2 (de 16 a 8 kHz): una sola fase, simétrica (`silk_Resampler_1_2_COEFS`).
pub(super) const RESAMPLER_1_2_COEFS: [i16; 2 + RESAMPLER_DOWN_ORDER_FIR1 / 2] = [
    616, -14_323, //
    -10, 39, 58, -46, -84, 120, 184, -315, -541, 1284, 5380, 9024,
];
