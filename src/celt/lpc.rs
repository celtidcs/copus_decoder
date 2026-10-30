//! Predicción lineal de CELT para la ocultación periódica (`celt/celt_lpc.c` de libopus 1.6.1, en
//! coma flotante): autocorrelación, coeficientes por Levinson-Durbin y los filtros FIR e IIR que
//! pasan de la señal a la excitación y vuelta.
//!
//! Las sumas largas van por los núcleos x86 de libopus emulados en [`super::x86_kernels`], en su
//! mismo orden, para dar los mismos coeficientes bit a bit.

use super::x86_kernels::{pitch_xcorr_avx2, xcorr_kernel_sse};

/// Bloque de muestras que filtra cada llamada a `xcorr_kernel` en `celt_fir` y `celt_iir`.
const KERNEL_BLOCK: usize = 4;
/// Energía mínima de la autocorrelación para calcular coeficientes (`ac[0] > 1e-10f`).
const MIN_AUTOCORR_ENERGY: f32 = 1e-10;
/// Ganancia de predicción con la que se para la recursión: 30 dB (`error <= .001f*ac[0]`).
const STOP_ERROR_FRACTION: f32 = 0.001;

/// `_celt_lpc`: coeficientes de predicción lineal de orden `ac.len() - 1` a partir de la
/// autocorrelación `ac`, por Levinson-Durbin; se para antes si la predicción ya gana 30 dB. Con
/// una autocorrelación casi nula, todos son 0.
pub(super) fn celt_lpc(ac: &[f32]) -> Vec<f32> {
    let p = ac.len() - 1;
    let mut lpc = vec![0.0f32; p];
    let mut error = ac[0];
    if ac[0] <= MIN_AUTOCORR_ENERGY {
        return lpc;
    }
    for i in 0..p {
        let mut rr = 0.0f32;
        for j in 0..i {
            rr += lpc[j] * ac[i - j];
        }
        rr += ac[i + 1];
        let r = -(rr / error);
        lpc[i] = r;
        for j in 0..(i + 1) >> 1 {
            let tmp1 = lpc[j];
            let tmp2 = lpc[i - 1 - j];
            lpc[j] = tmp1 + r * tmp2;
            lpc[i - 1 - j] = tmp2 + r * tmp1;
        }
        error -= (r * r) * error;
        if error <= STOP_ERROR_FRACTION * ac[0] {
            break;
        }
    }
    lpc
}

/// `celt_fir_c`: filtra `n` muestras de `signal` desde `start` con el FIR `1 + Σ num[k]·z^-(k+1)`
/// y devuelve el resultado. Necesita `num.len()` muestras de historia antes de `start`.
///
/// # Pánicos
///
/// Si `start < num.len()` o `signal` no llega a `start + n`.
pub(super) fn celt_fir(signal: &[f32], start: usize, num: &[f32], n: usize) -> Vec<f32> {
    let ord = num.len();
    let rnum: Vec<f32> = num.iter().rev().copied().collect();
    let mut y = vec![0.0f32; n];
    let mut i = 0usize;
    while i + KERNEL_BLOCK - 1 < n {
        let at = start + i;
        let mut sum = [signal[at], signal[at + 1], signal[at + 2], signal[at + 3]];
        xcorr_kernel_sse(&rnum, &signal[at - ord..], &mut sum, ord);
        y[i..i + KERNEL_BLOCK].copy_from_slice(&sum);
        i += KERNEL_BLOCK;
    }
    for (k, out) in y.iter_mut().enumerate().skip(i) {
        let at = start + k;
        let mut sum = signal[at];
        for (j, &coef) in rnum.iter().enumerate() {
            sum += coef * signal[at + j - ord];
        }
        *out = sum;
    }
    y
}

/// `celt_iir`: filtra `x` con el IIR `1 / (1 + Σ den[k]·z^-(k+1))`, partiendo de las salidas
/// anteriores `mem` (la más reciente primero), y devuelve la salida. Calcula de cuatro en cuatro
/// como un FIR y corrige después los términos que dependen de las salidas del propio bloque, como
/// libopus.
pub(super) fn celt_iir(x: &[f32], den: &[f32], mem: &[f32]) -> Vec<f32> {
    let ord = den.len();
    let n = x.len();
    let rden: Vec<f32> = den.iter().rev().copied().collect();
    // Salidas cambiadas de signo, con la historia delante.
    let mut history = vec![0.0f32; n + ord];
    for i in 0..ord {
        history[i] = -mem[ord - 1 - i];
    }
    let mut y = vec![0.0f32; n];
    let mut i = 0usize;
    while i + KERNEL_BLOCK - 1 < n {
        let mut sum = [x[i], x[i + 1], x[i + 2], x[i + 3]];
        xcorr_kernel_sse(&rden, &history[i..], &mut sum, ord);
        history[i + ord] = -sum[0];
        y[i] = sum[0];
        sum[1] += history[i + ord] * den[0];
        history[i + ord + 1] = -sum[1];
        y[i + 1] = sum[1];
        sum[2] += history[i + ord + 1] * den[0];
        sum[2] += history[i + ord] * den[1];
        history[i + ord + 2] = -sum[2];
        y[i + 2] = sum[2];
        sum[3] += history[i + ord + 2] * den[0];
        sum[3] += history[i + ord + 1] * den[1];
        sum[3] += history[i + ord] * den[2];
        history[i + ord + 3] = -sum[3];
        y[i + 3] = sum[3];
        i += KERNEL_BLOCK;
    }
    // Las muestras que no llenan un bloque, como libopus (que aquí guarda la salida sin cambiar
    // de signo). La ocultación siempre filtra un múltiplo de 4.
    for k in i..n {
        let mut sum = x[k];
        for (j, &coef) in rden.iter().enumerate() {
            sum -= coef * history[k + j];
        }
        history[k + ord] = sum;
        y[k] = sum;
    }
    y
}

/// `_celt_autocorr`: autocorrelación de `x` para los retardos `0..=lag`, con las `window.len()`
/// primeras y últimas muestras multiplicadas por la ventana.
///
/// # Pánicos
///
/// Si `x` tiene `lag` muestras o menos, o menos que el doble de la ventana.
pub(super) fn celt_autocorr(x: &[f32], window: &[f32], lag: usize) -> Vec<f32> {
    let n = x.len();
    let mut xx = x.to_vec();
    for (i, &w) in window.iter().enumerate() {
        xx[i] = x[i] * w;
        xx[n - i - 1] = x[n - i - 1] * w;
    }
    let fast_n = n - lag;
    let mut ac = vec![0.0f32; lag + 1];
    pitch_xcorr_avx2(&xx, &xx, &mut ac, fast_n, lag + 1);
    for (k, value) in ac.iter_mut().enumerate() {
        let mut d = 0.0f32;
        for i in k + fast_n..n {
            d += xx[i] * xx[i - k];
        }
        *value += d;
    }
    ac
}

#[cfg(test)]
mod tests {
    use super::{celt_autocorr, celt_fir, celt_iir, celt_lpc};

    /// Coeficientes de un AR(2) estable: `x[n] = 1,2·x[n-1] - 0,5·x[n-2] + e[n]`, que en la
    /// convención de libopus (`1 + Σ a·z^-k`) son `[-1,2, 0,5]`.
    const AR2: [f32; 2] = [-1.2, 0.5];
    const SAMPLES: usize = 4096;
    /// Tolerancia de la estimación de los coeficientes con una señal finita.
    const AR_TOLERANCE: f32 = 0.05;

    /// Ruido determinista uniforme en `[-1, 1)`.
    fn noise(n: usize) -> Vec<f32> {
        let mut seed = 12_345u32;
        (0..n)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 8) as f32 / (1u32 << 23) as f32 - 1.0
            })
            .collect()
    }

    #[test]
    fn levinson_recovers_a_known_autoregressive_process() {
        let e = noise(SAMPLES);
        let mut x = vec![0.0f32; SAMPLES];
        for n in 2..SAMPLES {
            x[n] = e[n] - AR2[0] * x[n - 1] - AR2[1] * x[n - 2];
        }
        let lpc = celt_lpc(&celt_autocorr(&x, &[], 2));
        for (got, want) in lpc.iter().zip(AR2) {
            assert!((got - want).abs() < AR_TOLERANCE, "{lpc:?}");
        }
    }

    /// El IIR deshace el FIR con los mismos coeficientes y la historia de la señal.
    #[test]
    fn iir_inverts_fir() {
        let order = 4;
        let x = noise(64 + order);
        let lpc = [-0.9f32, 0.4, -0.1, 0.05];
        let excitation = celt_fir(&x, order, &lpc, 64);
        let mem: Vec<f32> = (0..order).map(|i| x[order - 1 - i]).collect();
        let back = celt_iir(&excitation, &lpc, &mem);
        for (a, b) in back.iter().zip(&x[order..]) {
            assert!((a - b).abs() < 1e-4, "{a} != {b}");
        }
    }

    /// Sin energía no hay predicción.
    #[test]
    fn silent_autocorrelation_gives_zero_coefficients() {
        assert_eq!(celt_lpc(&[0.0; 5]), vec![0.0; 4]);
    }
}
