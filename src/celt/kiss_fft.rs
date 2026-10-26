//! Transformada discreta de Fourier directa que usa la MDCT inversa de CELT.
//!
//! Se calcula por la definición, en `f64`, sobre búferes complejos entrelazados
//! `[re0, im0, re1, im1, ...]`. No aplica escalado: la MDCT inversa de libopus
//! usa una FFT sin escalar.

use core::f32::consts::PI;

/// DFT directa de `input`, de `n = input.len() / 2` muestras complejas, en `output`; los dos en
/// formato `[re0, im0, re1, im1, ...]`. Escribe los `min(n, output.len() / 2)` primeros bins.
pub(crate) fn flat_fft_forward(input: &[f32], output: &mut [f32]) {
    let n = input.len() / 2;
    for (k, bin) in output.chunks_exact_mut(2).take(n).enumerate() {
        let mut sum_re = 0.0f64;
        let mut sum_im = 0.0f64;
        for j in 0..n {
            let angle = -2.0 * PI as f64 * (k as f64) * (j as f64) / (n as f64);
            let (sin_a, cos_a) = angle.sin_cos();
            let re = input[2 * j] as f64;
            let im = input[2 * j + 1] as f64;
            sum_re += re * cos_a - im * sin_a;
            sum_im += re * sin_a + im * cos_a;
        }
        bin[0] = sum_re as f32;
        bin[1] = sum_im as f32;
    }
}

#[cfg(test)]
mod tests {
    use super::flat_fft_forward;
    use core::f32::consts::PI;

    /// Tamaño de la transformada de prueba: 60, el `N/4` de la MDCT corta de CELT.
    const N: usize = 60;
    /// Tolerancia absoluta frente al valor exacto, holgada para la suma en `f64` convertida a `f32`.
    const TOLERANCIA: f32 = 1e-3;

    /// Comprueba que la DFT directa reproduce dos casos con resultado conocido:
    /// un impulso (espectro plano de valor 1) y una exponencial compleja de
    /// frecuencia 3 (toda la energía, `N`, en el bin 3 y cero en los demás).
    #[test]
    fn la_dft_directa_reproduce_impulso_y_tono() {
        let mut impulso = vec![0.0f32; 2 * N];
        impulso[0] = 1.0;
        let mut salida = vec![0.0f32; 2 * N];
        flat_fft_forward(&impulso, &mut salida);
        for k in 0..N {
            assert!(
                (salida[2 * k] - 1.0).abs() < TOLERANCIA,
                "impulso, bin {k}: re"
            );
            assert!(salida[2 * k + 1].abs() < TOLERANCIA, "impulso, bin {k}: im");
        }

        let frecuencia = 3usize;
        let mut tono = vec![0.0f32; 2 * N];
        for j in 0..N {
            let fase = 2.0 * PI * (frecuencia * j) as f32 / N as f32;
            tono[2 * j] = fase.cos();
            tono[2 * j + 1] = fase.sin();
        }
        flat_fft_forward(&tono, &mut salida);
        for k in 0..N {
            let esperado = if k == frecuencia { N as f32 } else { 0.0 };
            assert!(
                (salida[2 * k] - esperado).abs() < TOLERANCIA,
                "tono, bin {k}: re"
            );
            assert!(salida[2 * k + 1].abs() < TOLERANCIA, "tono, bin {k}: im");
        }
    }
}
