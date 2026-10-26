//! Formato de la salida PCM, común a CELT y al decodificador de modos: frecuencias admitidas y
//! conversión de muestras en coma flotante a `i16`.

/// Frecuencias de salida admitidas (RFC 6716 §2).
pub(crate) const SUPPORTED_RATES_HZ: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];

/// Redondea una muestra a entero y la satura al rango de `i16`.
pub(crate) fn saturate_to_i16(sample: f32) -> i16 {
    sample.round().clamp(i16::MIN as f32, i16::MAX as f32) as i16
}
