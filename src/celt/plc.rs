//! Ocultación de pérdidas de CELT y su efecto en la trama buena siguiente (`celt_decode_lost` y
//! el principio de `celt_decode_with_ec` de `celt/celt_decoder.c` en libopus 1.6.1).
//!
//! Aquí solo hay cálculos sin estado propio: la caída de energía de la trama oculta con ruido, su
//! espectro, la extrapolación periódica de la memoria de síntesis y el ajuste que hace segura la
//! predicción de energía de la trama buena que sigue a una pérdida.

use std::cmp::Ordering;
use std::ops::Range;

use super::DECODE_BUFFER_SIZE;
use super::bands::celt_lcg_rand;
use super::lpc::{celt_autocorr, celt_fir, celt_iir, celt_lpc};
use super::vq::renormalise_vector;

/// Tope de la duración de las pérdidas, en bloques cortos, para que no desborde (`IMIN(10000,
/// ...)`).
pub(super) const LOSS_DURATION_MAX: u32 = 10_000;
/// Caída de la energía, en log2, de la primera trama oculta tras una buena y de las siguientes
/// (`decay = loss_duration==0 ? 1.5 : .5`).
const FIRST_LOSS_DECAY: f32 = 1.5;
const NEXT_LOSS_DECAY: f32 = 0.5;
/// Desplazamiento de la semilla con signo que da cada muestra del ruido
/// (`(opus_int32)seed>>20`).
const NOISE_SEED_SHIFT: u32 = 20;
/// Pérdidas recordadas como mucho, en tramas, al hacer segura la predicción de energía
/// (`IMIN(10, loss_duration>>LM)`).
const MAX_MISSING_FRAMES: u32 = 10;
/// Margen que se resta a la energía predicha tras una pérdida en tramas de 2,5 y 5 ms, que
/// fluctúan más (`safety` con `LM == 0` y `LM == 1`).
const SAFETY_LM0: f32 = 1.5;
const SAFETY_LM1: f32 = 0.5;
/// Pendiente máxima de caída de energía que se prolonga, y energía mínima resultante
/// (`MING(slope, 2.f)` y `MAX32(-20.f, E0)`).
const MAX_DECAY_SLOPE: f32 = 2.0;
const MIN_PREDICTED_ENERGY: f32 = -20.0;

/// Duración de las pérdidas seguidas, en bloques cortos, desde la que se oculta con ruido en vez
/// de repetir el periodo de tono (`plc_duration >= 40`: 100 ms).
pub(super) const MAX_PERIODIC_PLC_DURATION: u32 = 40;
/// Muestras de señal pasada de las que sale la excitación (`MAX_PERIOD`).
const MAX_PERIOD: usize = 1024;
/// Orden de la predicción lineal de la ocultación periódica (`CELT_LPC_ORDER`).
pub(super) const LPC_ORDER: usize = 24;
/// Suelo de ruido de −40 dB que se suma a la energía antes de calcular la predicción, y ventana
/// de retardo (`ac[0] *= 1.0001f` y `ac[i] -= ac[i]*(0.008f*0.008f)*i*i`).
const NOISE_FLOOR_GAIN: f32 = 1.0001;
const LAG_WINDOW: f32 = 0.008 * 0.008;
/// Atenuación de cada pérdida periódica después de la primera (`fade = .8`).
pub(super) const REPEATED_LOSS_FADE: f32 = 0.8;
/// La síntesis «explota» si su energía pasa de cinco veces la de la señal que copia
/// (`!(S1 > 0.2f*S2)`): entonces se anula.
const EXPLOSION_RATIO: f32 = 0.2;

/// Parámetros de la extrapolación periódica de un canal.
pub(super) struct PeriodicConcealment<'a> {
    /// Ventana del solapamiento de la MDCT, con `overlap` muestras.
    pub window: &'a [f32],
    /// Periodo de tono, en muestras a 48 kHz.
    pub pitch: usize,
    /// Atenuación adicional: 1 en la primera pérdida y [`REPEATED_LOSS_FADE`] en las siguientes.
    pub fade: f32,
    /// Duración de la trama oculta a 48 kHz.
    pub frame_size_48k: usize,
    /// Si se calcula la predicción lineal (solo en la primera pérdida; si no, se reutiliza).
    pub recompute_lpc: bool,
}

/// Rama periódica de `celt_decode_lost` para la memoria de síntesis `mem` de un canal: pasa los
/// últimos `MAX_PERIOD` valores a excitación con la predicción `lpc` (que calcula en la primera
/// pérdida), la repite con el periodo de tono y una caída por periodo, la devuelve a señal con el
/// filtro inverso y la atenúa si su energía crece. Desplaza la memoria una trama y escribe la
/// trama oculta y su solapamiento al final.
pub(super) fn conceal_periodic(
    mem: &mut [f32],
    lpc: &mut [f32; LPC_ORDER],
    params: &PeriodicConcealment<'_>,
) {
    let dbs = DECODE_BUFFER_SIZE;
    let n = params.frame_size_48k;
    let pitch = params.pitch;
    let window = params.window;
    let overlap = window.len();
    let exc_length = (2 * pitch).min(MAX_PERIOD);

    // Excitación con la historia de la predicción delante: `exc[LPC_ORDER + k]` es la muestra
    // `k` de los últimos `MAX_PERIOD`.
    let mut exc = mem[dbs - MAX_PERIOD - LPC_ORDER..dbs].to_vec();
    if params.recompute_lpc {
        let mut ac = celt_autocorr(&exc[LPC_ORDER..], window, LPC_ORDER);
        ac[0] *= NOISE_FLOOR_GAIN;
        for (i, value) in ac.iter_mut().enumerate().skip(1) {
            *value -= *value * LAG_WINDOW * i as f32 * i as f32;
        }
        lpc.copy_from_slice(&celt_lpc(&ac));
    }
    let exc_start = LPC_ORDER + MAX_PERIOD - exc_length;
    let filtered = celt_fir(&exc, exc_start, lpc, exc_length);
    exc[exc_start..exc_start + exc_length].copy_from_slice(&filtered);

    // Si la excitación ya decaía, la extrapolación sigue decayendo al mismo ritmo.
    let decay_length = exc_length >> 1;
    let end = LPC_ORDER + MAX_PERIOD;
    let (mut e1, mut e2) = (1.0f32, 1.0f32);
    for i in 0..decay_length {
        let e = exc[end - decay_length + i];
        e1 += e * e;
        let e = exc[end - 2 * decay_length + i];
        e2 += e * e;
    }
    let decay = (e1.min(e2) / e2).sqrt();

    mem.copy_within(n..dbs, 0);
    let extrapolation_offset = MAX_PERIOD - pitch;
    let extrapolation_len = n + overlap;
    let out = dbs - n;
    let mut attenuation = params.fade * decay;
    let mut s1 = 0.0f32;
    let mut j = 0usize;
    for i in 0..extrapolation_len {
        if j >= pitch {
            j -= pitch;
            attenuation *= decay;
        }
        mem[out + i] = attenuation * exc[LPC_ORDER + extrapolation_offset + j];
        // Energía de la señal descodificada cuya excitación se copia.
        let source = mem[dbs - MAX_PERIOD - n + extrapolation_offset + j];
        s1 += source * source;
        j += 1;
    }

    let lpc_mem: Vec<f32> = (0..LPC_ORDER).map(|i| mem[out - 1 - i]).collect();
    let synthesis = celt_iir(&mem[out..out + extrapolation_len], lpc, &lpc_mem);
    mem[out..out + extrapolation_len].copy_from_slice(&synthesis);

    let s2: f32 = mem[out..out + extrapolation_len]
        .iter()
        .fold(0.0f32, |acc, &v| acc + v * v);
    // Explota si `s1` no es mayor: también si la síntesis dio un NaN, como en libopus
    // (`!(S1 > 0.2f*S2)`).
    let explodes = s1.partial_cmp(&(EXPLOSION_RATIO * s2)) != Some(Ordering::Greater);
    if explodes {
        mem[out..out + extrapolation_len].fill(0.0);
    } else if s1 < s2 {
        let ratio = ((s1 + 1.0) / (s2 + 1.0)).sqrt();
        for (value, &w) in mem[out..out + overlap].iter_mut().zip(window) {
            *value *= 1.0 - w * (1.0 - ratio);
        }
        for value in &mut mem[out + overlap..out + extrapolation_len] {
            *value *= ratio;
        }
    }
}

/// Caída de energía de la trama oculta: mayor en la primera tras una buena.
pub(super) fn noise_decay(loss_duration: u32) -> f32 {
    if loss_duration == 0 {
        FIRST_LOSS_DECAY
    } else {
        NEXT_LOSS_DECAY
    }
}

/// Baja la energía `energy` de las bandas `bands` en `decay` sin pasar de la de fondo
/// `background` (`oldBandE = MAXG(backgroundLogE, oldBandE - decay)`).
pub(super) fn decay_energy(
    energy: &mut [f32],
    background: &[f32],
    bands: Range<usize>,
    decay: f32,
) {
    for i in bands {
        energy[i] = background[i].max(energy[i] - decay);
    }
}

/// Llena en `spectrum` las bandas `bands` de ruido normalizado, cada una con norma 1, a partir
/// de `seed`, y devuelve la semilla siguiente. `e_bands` da los límites de las bandas en bloques
/// cortos y `lm` el número de bloques por trama.
pub(super) fn fill_noise(
    spectrum: &mut [f32],
    e_bands: &[i16],
    bands: Range<usize>,
    lm: usize,
    mut seed: u32,
) -> u32 {
    for i in bands {
        let band = (e_bands[i] as usize) << lm..(e_bands[i + 1] as usize) << lm;
        for value in &mut spectrum[band.clone()] {
            seed = celt_lcg_rand(seed);
            *value = ((seed as i32) >> NOISE_SEED_SHIFT) as f32;
        }
        renormalise_vector(&mut spectrum[band], 1.0);
    }
    seed
}

/// Historial de energía de un canal tras la última trama: la última (`oldBandE`) y las dos
/// anteriores ya guardadas (`oldLogE` y `oldLogE2`).
pub(super) struct EnergyHistory<'a> {
    pub last: &'a mut [f32],
    pub previous: &'a [f32],
    pub before_previous: &'a [f32],
}

/// Hace segura la predicción de energía de la primera trama buena tras `loss_duration` bloques
/// cortos perdidos, para no arriesgar artefactos fuertes: si la energía de una banda ya bajaba,
/// prolonga la caída tantas tramas como faltan; si no, toma la menor de las tres últimas. Después
/// resta un margen en las tramas cortas.
pub(super) fn make_energy_prediction_safe(
    history: EnergyHistory<'_>,
    bands: Range<usize>,
    lm: usize,
    loss_duration: u32,
) {
    let missing = MAX_MISSING_FRAMES.min(loss_duration >> lm) as f32;
    let safety = match lm {
        0 => SAFETY_LM0,
        1 => SAFETY_LM1,
        _ => 0.0,
    };
    for i in bands {
        let e0 = history.last[i];
        let e1 = history.previous[i];
        let e2 = history.before_previous[i];
        let predicted = if e0 < e1.max(e2) {
            let slope = (e1 - e0).max(0.5 * (e2 - e0)).min(MAX_DECAY_SLOPE);
            (e0 - ((1.0 + missing) * slope).max(0.0)).max(MIN_PREDICTED_ENERGY)
        } else {
            e0.min(e1).min(e2)
        };
        history.last[i] = predicted - safety;
    }
}

#[cfg(test)]
mod tests {
    use super::{EnergyHistory, make_energy_prediction_safe, noise_decay};

    /// Energías (log2) de tres tramas seguidas de una banda.
    const BEFORE_PREVIOUS: f32 = 6.0;
    const PREVIOUS: f32 = 5.0;
    const LAST_FALLING: f32 = 4.0;
    const LAST_RISING: f32 = 7.0;
    /// Bloques cortos perdidos: dos tramas de 20 ms (`LM == 3`).
    const LOSS_TWO_20MS_FRAMES: u32 = 16;
    const LM_20MS: usize = 3;
    const LM_2_5MS: usize = 0;

    fn safe_prediction(last: f32, lm: usize, loss_duration: u32) -> f32 {
        let mut energy = [last];
        make_energy_prediction_safe(
            EnergyHistory {
                last: &mut energy,
                previous: &[PREVIOUS],
                before_previous: &[BEFORE_PREVIOUS],
            },
            0..1,
            lm,
            loss_duration,
        );
        energy[0]
    }

    /// Como libopus: si la energía bajaba, la pendiente (la mayor entre la última caída y la
    /// mitad de las dos últimas) se prolonga `1 + tramas perdidas` veces.
    #[test]
    fn falling_energy_keeps_falling_for_each_missing_frame() {
        let slope = (PREVIOUS - LAST_FALLING).max(0.5 * (BEFORE_PREVIOUS - LAST_FALLING));
        let expected = LAST_FALLING - 3.0 * slope;

        assert_eq!(
            safe_prediction(LAST_FALLING, LM_20MS, LOSS_TWO_20MS_FRAMES),
            expected
        );
    }

    /// Si no bajaba, la menor de las tres, menos el margen de las tramas de 2,5 ms.
    #[test]
    fn rising_energy_takes_the_minimum_minus_the_short_frame_margin() {
        let expected = PREVIOUS - 1.5;

        assert_eq!(safe_prediction(LAST_RISING, LM_2_5MS, 1), expected);
    }

    /// La primera trama oculta tras una buena cae más que las siguientes.
    #[test]
    fn first_concealed_frame_decays_more() {
        assert_eq!(noise_decay(0), 1.5);
        assert_eq!(noise_decay(LOSS_TWO_20MS_FRAMES), 0.5);
    }
}
