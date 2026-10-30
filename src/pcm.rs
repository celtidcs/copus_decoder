//! Formato de la salida PCM, común a CELT y al decodificador de modos: frecuencias admitidas y
//! conversión de muestras en coma flotante a `i16`.

/// Frecuencias de salida admitidas (RFC 6716 §2).
pub(crate) const SUPPORTED_RATES_HZ: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];

/// Redondea una muestra a entero, con los empates al par, y la satura al rango de `i16`, como
/// `FLOAT2INT16` de libopus (`celt/float_cast.h`).
pub(crate) fn saturate_to_i16(sample: f32) -> i16 {
    sample
        .round_ties_even()
        .clamp(i16::MIN as f32, i16::MAX as f32) as i16
}

#[cfg(test)]
mod tests {
    use super::saturate_to_i16;

    /// Como `FLOAT2INT16` de libopus (`celt/float_cast.h`), los empates se redondean al par
    /// (`cvtss2si` o `lrintf` en el modo de redondeo por defecto), no lejos del cero.
    #[test]
    fn los_empates_se_redondean_al_par() {
        let casos = [
            (0.5, 0),
            (1.5, 2),
            (2.5, 2),
            (-0.5, 0),
            (-1.5, -2),
            (-2.5, -2),
            (32_766.5, 32_766),
            (-32_767.5, -32_768),
        ];
        for (muestra, esperado) in casos {
            assert_eq!(saturate_to_i16(muestra), esperado, "muestra {muestra}");
        }
    }

    /// Fuera del rango de `i16` satura igual que `FLOAT2INT16`, que satura antes de redondear.
    #[test]
    fn fuera_de_rango_satura() {
        let casos = [
            (40_000.0, i16::MAX),
            (32_767.5, i16::MAX),
            (-40_000.0, i16::MIN),
            (-32_768.5, i16::MIN),
        ];
        for (muestra, esperado) in casos {
            assert_eq!(saturate_to_i16(muestra), esperado, "muestra {muestra}");
        }
    }
}
