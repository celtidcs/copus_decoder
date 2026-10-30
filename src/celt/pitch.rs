//! Búsqueda del periodo de tono para la ocultación periódica de CELT (`celt/pitch.c` y
//! `celt_plc_pitch_search` de `celt/celt_decoder.c` en libopus 1.6.1, en coma flotante): diezma
//! la memoria de síntesis con un filtro blanqueador y busca la correlación máxima, primero con
//! diezmado 4 y después afinando con diezmado 2.

use super::lpc::{celt_autocorr, celt_lpc};
use super::x86_kernels::{inner_prod_sse, pitch_xcorr_avx2};

/// Retardos máximo y mínimo, en muestras a 48 kHz, que busca la ocultación
/// (`PLC_PITCH_LAG_MAX` y `PLC_PITCH_LAG_MIN`).
pub(super) const PLC_PITCH_LAG_MAX: usize = 720;
const PLC_PITCH_LAG_MIN: usize = 100;
/// Orden de la predicción del blanqueador de `pitch_downsample`.
const DOWNSAMPLE_LPC_ORDER: usize = 4;
/// Suelo de ruido de −40 dB que se suma a la energía antes de calcular la predicción.
const NOISE_FLOOR_GAIN: f32 = 1.0001;
/// Ventana de retardo de la autocorrelación (`ac[i] -= ac[i]*(.008f*i)*(.008f*i)`).
const LAG_WINDOW_STEP: f32 = 0.008;
/// Expansión de ancho de banda de la predicción (`.9^k`) y cero añadido en `0,8`.
const BANDWIDTH_EXPANSION: f32 = 0.9;
const ADDED_ZERO: f32 = 0.8;
/// Escala que evita desbordar al elevar al cuadrado la correlación (`xcorr16 *= 1e-12f`).
const XCORR_SCALE: f32 = 1e-12;
/// Distancia, en muestras diezmadas 2, alrededor de los dos mejores periodos gruesos que se
/// afina, y umbral de la interpolación final (`.7`).
const FINE_SEARCH_RADIUS: usize = 2;
const INTERPOLATION_THRESHOLD: f32 = 0.7;

/// `celt_plc_pitch_search`: periodo de tono, en muestras a 48 kHz, de la memoria de síntesis de
/// los canales `channels` (cada uno de `2 · half_len` muestras).
pub(super) fn plc_pitch_search(channels: &[&[f32]], half_len: usize) -> usize {
    let lp = pitch_downsample(channels, half_len);
    let search_len = 2 * half_len - PLC_PITCH_LAG_MAX;
    let index = pitch_search(
        &lp[PLC_PITCH_LAG_MAX >> 1..],
        &lp,
        search_len,
        PLC_PITCH_LAG_MAX - PLC_PITCH_LAG_MIN,
    );
    PLC_PITCH_LAG_MAX - index
}

/// `pitch_downsample` con factor 2: media ponderada `1/4, 1/2, 1/4` de los canales y filtro
/// blanqueador de orden 4 con un cero añadido.
fn pitch_downsample(channels: &[&[f32]], len: usize) -> Vec<f32> {
    let mut x_lp = vec![0.0f32; len];
    for (c, x) in channels.iter().enumerate() {
        for i in 1..len {
            let value = 0.25 * x[2 * i - 1] + 0.25 * x[2 * i + 1] + 0.5 * x[2 * i];
            x_lp[i] = if c == 0 { value } else { x_lp[i] + value };
        }
        let first = 0.25 * x[1] + 0.5 * x[0];
        x_lp[0] = if c == 0 { first } else { x_lp[0] + first };
    }

    let mut ac = celt_autocorr(&x_lp, &[], DOWNSAMPLE_LPC_ORDER);
    ac[0] *= NOISE_FLOOR_GAIN;
    for (i, value) in ac.iter_mut().enumerate().skip(1) {
        let step = LAG_WINDOW_STEP * i as f32;
        *value -= *value * step * step;
    }
    let mut lpc = celt_lpc(&ac);
    let mut tmp = 1.0f32;
    for coef in &mut lpc {
        tmp *= BANDWIDTH_EXPANSION;
        *coef *= tmp;
    }
    let lpc2 = [
        lpc[0] + ADDED_ZERO,
        lpc[1] + ADDED_ZERO * lpc[0],
        lpc[2] + ADDED_ZERO * lpc[1],
        lpc[3] + ADDED_ZERO * lpc[2],
        ADDED_ZERO * lpc[3],
    ];
    celt_fir5(&mut x_lp, &lpc2);
    x_lp
}

/// `celt_fir5`: FIR de orden 5 en el sitio, `x[i] + Σ num[k]·x[i-k-1]`.
fn celt_fir5(x: &mut [f32], num: &[f32; 5]) {
    let mut mem = [0.0f32; 5];
    for value in x.iter_mut() {
        let mut sum = *value;
        for (coef, past) in num.iter().zip(&mem) {
            sum += coef * past;
        }
        mem.rotate_right(1);
        mem[0] = *value;
        *value = sum;
    }
}

/// `pitch_search`: retardo de `y` respecto a `x_lp` en `0..max_pitch` con la mayor correlación
/// normalizada, de `len` muestras: búsqueda gruesa con diezmado 4, fina con diezmado 2 junto a los
/// dos mejores y media muestra de interpolación.
fn pitch_search(x_lp: &[f32], y: &[f32], len: usize, max_pitch: usize) -> usize {
    let lag = len + max_pitch;
    let x_lp4: Vec<f32> = x_lp.iter().step_by(2).take(len >> 2).copied().collect();
    let y_lp4: Vec<f32> = y.iter().step_by(2).take(lag >> 2).copied().collect();

    let mut xcorr = vec![0.0f32; max_pitch >> 1];
    pitch_xcorr_avx2(&x_lp4, &y_lp4, &mut xcorr, len >> 2, max_pitch >> 2);
    let coarse = find_best_pitch(&xcorr, &y_lp4, len >> 2, max_pitch >> 2);

    for (i, value) in xcorr.iter_mut().enumerate() {
        *value = 0.0;
        if i.abs_diff(2 * coarse[0]) > FINE_SEARCH_RADIUS
            && i.abs_diff(2 * coarse[1]) > FINE_SEARCH_RADIUS
        {
            continue;
        }
        *value = inner_prod_sse(x_lp, &y[i..], len >> 1).max(-1.0);
    }
    let best = find_best_pitch(&xcorr, y, len >> 1, max_pitch >> 1)[0];

    let offset = if best > 0 && best < (max_pitch >> 1) - 1 {
        let (a, b, c) = (xcorr[best - 1], xcorr[best], xcorr[best + 1]);
        if c - a > INTERPOLATION_THRESHOLD * (b - a) {
            1
        } else if a - c > INTERPOLATION_THRESHOLD * (b - c) {
            -1
        } else {
            0
        }
    } else {
        0
    };
    (2 * best as isize - offset) as usize
}

/// `find_best_pitch`: los dos retardos de `0..max_pitch` con mayor `xcorr²/Syy`, donde `Syy` es la
/// energía de la ventana de `len` muestras de `y` en ese retardo; solo cuentan las correlaciones
/// positivas.
fn find_best_pitch(xcorr: &[f32], y: &[f32], len: usize, max_pitch: usize) -> [usize; 2] {
    let mut syy = 1.0f32;
    let mut best_num = [-1.0f32; 2];
    let mut best_den = [0.0f32; 2];
    let mut best_pitch = [0usize, 1];
    for value in &y[..len] {
        syy += value * value;
    }
    for i in 0..max_pitch {
        if xcorr[i] > 0.0 {
            let xcorr16 = xcorr[i] * XCORR_SCALE;
            let num = xcorr16 * xcorr16;
            if num * best_den[1] > best_num[1] * syy {
                if num * best_den[0] > best_num[0] * syy {
                    best_num[1] = best_num[0];
                    best_den[1] = best_den[0];
                    best_pitch[1] = best_pitch[0];
                    best_num[0] = num;
                    best_den[0] = syy;
                    best_pitch[0] = i;
                } else {
                    best_num[1] = num;
                    best_den[1] = syy;
                    best_pitch[1] = i;
                }
            }
        }
        syy += y[i + len] * y[i + len] - y[i] * y[i];
        syy = syy.max(1.0);
    }
    best_pitch
}

#[cfg(test)]
mod tests {
    use super::plc_pitch_search;

    /// Mitad de la memoria de síntesis que diezma la ocultación (`DECODE_BUFFER_SIZE >> 1`).
    const HALF_LEN: usize = 1024;

    /// Un tren de pulsos con periodo conocido da ese periodo. Los periodos pasan de la mitad del
    /// retardo máximo (720), para que ningún múltiplo suyo empate dentro del margen.
    #[test]
    fn finds_the_period_of_a_pulse_train() {
        for period in [400usize, 500, 650] {
            let signal: Vec<f32> = (0..2 * HALF_LEN)
                .map(|n| if n % period == 0 { 10_000.0 } else { 0.0 })
                .collect();
            let found = plc_pitch_search(&[&signal], HALF_LEN);
            assert!(found.abs_diff(period) <= 1, "periodo {period}: {found}");
        }
    }
}
