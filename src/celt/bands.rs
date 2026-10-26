//! Decodificación de las bandas de CELT (`bands.c`): particiones PVQ, estéreo, plegado,
//! anticolapso y desnormalización.

use std::borrow::Cow;
use std::ops::Range;

use crate::celt::modes::CeltMode;
use crate::celt::quant_bands::{self, EnergyFrame};
use crate::celt::rate;
use crate::celt::vq;
use crate::entropy::{BITRES, EcDec, ec_ilog};

const QTHETA_OFFSET: i32 = 4;
const QTHETA_OFFSET_TWOPHASE: i32 = 16;
const SPREAD_AGGRESSIVE: i32 = 3;
const BIT_INTERLEAVE_TABLE: [u32; 16] = [0, 1, 1, 1, 2, 3, 3, 3, 2, 3, 3, 3, 2, 3, 3, 3];
const BIT_DEINTERLEAVE_TABLE: [u8; 16] = [
    0x00, 0x03, 0x0C, 0x0F, 0x30, 0x33, 0x3C, 0x3F, 0xC0, 0xC3, 0xCC, 0xCF, 0xF0, 0xF3, 0xFC, 0xFF,
];
const EXP2_TABLE8: [i32; 8] = [16384, 17866, 19483, 21247, 23170, 25267, 27554, 30048];
const ORDERY_TABLE: [usize; 30] = [
    1, 0, 3, 0, 2, 1, 7, 0, 4, 3, 6, 1, 5, 2, 15, 0, 8, 7, 12, 3, 11, 4, 14, 1, 9, 6, 13, 2, 10, 5,
];
/// Multiplicador y sumando del generador congruencial de CELT (`celt_lcg_rand` de libopus).
const LCG_MULTIPLIER: u32 = 1_664_525;
const LCG_INCREMENT: u32 = 1_013_904_223;
/// Bit del generador que decide el signo del ruido del anticolapso y del plegado (`seed&0x8000`).
const LCG_SIGN_BIT: u32 = 0x8000;
/// Desplazamiento que deja el ruido del relleno a partir de la semilla (`seed>>20` en
/// `quant_partition`).
const NOISE_FILL_SHIFT: i32 = 20;
/// Amplitud del ruido añadido al plegar una banda: 1/256 (`QCONST16(1.0f/256, ...)` en
/// `quant_partition`).
const FOLD_NOISE_LEVEL: f32 = 1.0 / 256.0;
/// LM máximo de CELT: tramas de 20 ms con 8 bloques cortos.
const MAX_LM: usize = 3;
/// Pendiente del umbral del anticolapso por cada nivel de profundidad
/// (`thresh = .5f*celt_exp2(-.125f*depth)` en `anti_collapse`).
const ANTI_COLLAPSE_DEPTH_SLOPE: f32 = 0.125;
/// Ángulo de la división mid/side en Q14: 16384 es un cuarto de vuelta (90°) y 8192, un octavo
/// (`compute_theta` y `quant_partition`).
const ITHETA_QUARTER_TURN: i32 = 16_384;
const ITHETA_EIGHTH_TURN: i32 = 8_192;
/// 1,0 en Q15, valor máximo de `bitexact_cos`, y la escala para pasarlo a coma flotante.
const Q15_ONE: i32 = 32_767;
const Q15_SCALE: f32 = 32_768.0;
/// Bits fraccionarios de Q15 y redondeo de `FRAC_MUL16` (`(16384 + a*b) >> 15`).
const Q15_SHIFT: i32 = 15;
const Q15_ROUND: i32 = 1 << (Q15_SHIFT - 1);
/// `bitexact_cos` eleva al cuadrado el ángulo en Q14 y lo deja en Q15 redondeando
/// (`(4096 + x*x) >> 13`).
const COS_SQUARE_SHIFT: i32 = 13;
const COS_SQUARE_ROUND: i32 = 1 << (COS_SQUARE_SHIFT - 1);
/// `bitexact_log2tan` da el logaritmo en Q11 (`(ls-lc)*(1<<11)`).
const LOG2TAN_Q: i32 = 11;
/// `EXP2_TABLE8` está en Q14 (`exp2_table8[qb&0x7]>>(14-(qb>>BITRES))` en `compute_qn`).
const EXP2_TABLE8_Q: i32 = 14;
/// Coeficientes de los polinomios de `bitexact_cos` y de `bitexact_log2tan` (libopus `bands.c`).
const BITEXACT_COS_COEFS: [i32; 3] = [-7_651, 8_277, -626];
const BITEXACT_LOG2TAN_COEFS: [i32; 2] = [-2_597, 7_932];
/// Bits reservados al resto de la banda y máximo de bits para la resolución de theta, en bits
/// enteros (`b-pulse_cap-(4<<BITRES)` y `8<<BITRES` en `compute_qn`).
const QN_RESERVED_BITS: i32 = 4;
const QN_MAX_BITS: i32 = 8;
/// Resolución máxima de theta: con `QN_MAX_BITS`, `exp2_table8` no pasa de 256.
const MAX_QN: i32 = 256;
/// Peso de los pasos de theta por debajo de la mitad en la distribución triangular del estéreo
/// (`p0 = 3` en `compute_theta`).
const THETA_STEREO_STEP_WEIGHT: i32 = 3;
/// Bits por encima de los cuales se reparte el sobrante entre mid y side (`rebalance > 3<<BITRES`
/// en `quant_partition` y `quant_band_stereo`).
const REBALANCE_THRESHOLD_BITS: i32 = 3;
/// Margen sobre la caché de pulsos a partir del cual una partición se divide en dos
/// (`b > cache[cache[0]]+12` en `quant_partition`).
const SPLIT_CACHE_MARGIN: i32 = 12;
/// Exponentes del ajuste de `delta` en las divisiones con bloques cortos
/// (`delta -= delta>>(4-LM)` y `N<<BITRES>>(5-LM)` en `quant_partition`).
const DELTA_DAMP_SHIFT_BASE: i32 = 4;
const DELTA_BIAS_SHIFT_BASE: i32 = 5;
/// Bandas entre las que se reparte el saldo de bits (`IMIN(3, codedBands-i)` en
/// `quant_all_bands`) y máximo de bits de una banda (`IMIN(16383, ...)`).
const BALANCE_SPREAD_BANDS: usize = 3;
const MAX_BAND_BITS: i32 = 16_383;
/// Energía mínima de cada canal para deshacer mid/side en `stereo_merge`; por debajo se copia el
/// canal medio.
const STEREO_MERGE_MIN_ENERGY: f32 = 6.0e-4;
/// Las tablas de entrelazado de bits indexan 4 bits.
const INTERLEAVE_NIBBLE_BITS: u32 = 4;
const INTERLEAVE_NIBBLE_MASK: u32 = 0x0F;
/// Cota superior del logaritmo de la ganancia al desnormalizar, como libopus en coma flotante
/// (`celt_exp2_db(MIN32(32.f, lg))` en `celt/bands.c`): la ganancia no pasa de 2^32.
const DENORMALISE_MAX_LOG_GAIN: f32 = 32.0;

/// Siguiente semilla del generador congruencial de CELT (`celt_lcg_rand`), del que salen el
/// ruido del relleno y del plegado.
pub(crate) fn celt_lcg_rand(seed: u32) -> u32 {
    seed.wrapping_mul(LCG_MULTIPLIER)
        .wrapping_add(LCG_INCREMENT)
}

/// Energía de la trama actual y de las dos anteriores, en log2, con `nb_ebands` posiciones por
/// canal.
pub(crate) struct EnergyHistory<'a> {
    pub(crate) current: &'a [f32],
    pub(crate) prev1: &'a [f32],
    pub(crate) prev2: &'a [f32],
}

/// Evita el colapso de energía en las tramas transitorias (`anti_collapse`): rellena con ruido
/// leve los bloques cortos que se quedaron sin pulsos según `collapse_masks`, con un nivel que
/// depende de los pulsos de la banda y de cuánto subió su energía. Modifica `x` (e `y`, si hay
/// dos canales codificados).
pub(crate) fn anti_collapse(
    frame: &EnergyFrame<'_>,
    x: &mut [f32],
    mut y: Option<&mut [f32]>,
    collapse_masks: &[u8],
    history: &EnergyHistory<'_>,
    pulses: &[i32],
    seed: u32,
) {
    let EnergyFrame {
        mode,
        start,
        end,
        channels: coded_channels,
        lm,
    } = *frame;
    let EnergyHistory {
        current: log_e,
        prev1: prev1_log_e,
        prev2: prev2_log_e,
    } = *history;
    let nb = mode.nb_ebands;
    if coded_channels == 0 || lm > MAX_LM {
        return;
    }
    let mut seed_local = seed;
    let blocks = 1usize << lm;
    let block_mask_limit = if blocks >= u8::BITS as usize {
        u8::MAX
    } else {
        ((1u16 << blocks) - 1) as u8
    };

    for i in start..end {
        let n0 = (mode.e_bands[i + 1] - mode.e_bands[i]) as usize;
        if n0 == 0 || i >= pulses.len() {
            continue;
        }
        let depth = (((1 + pulses[i].max(0)) as usize) / n0) >> lm;
        let thresh = 0.5 * (2.0f32).powf(-ANTI_COLLAPSE_DEPTH_SLOPE * depth as f32);
        let sqrt_1 = 1.0 / ((n0 << lm) as f32).sqrt();
        let band_start = (mode.e_bands[i] as usize) << lm;
        let band_len = n0 << lm;

        for c in 0..coded_channels.min(2) {
            let chan = if c == 0 {
                &mut x[..]
            } else if let Some(yv) = y.as_deref_mut() {
                yv
            } else {
                continue;
            };
            if band_start + band_len > chan.len() {
                continue;
            }
            let mut prev1 = prev1_log_e[c * nb + i];
            let mut prev2 = prev2_log_e[c * nb + i];
            if coded_channels == 1 {
                prev1 = prev1.max(prev1_log_e[nb + i]);
                prev2 = prev2.max(prev2_log_e[nb + i]);
            }
            let ediff = (log_e[c * nb + i] - prev1.min(prev2)).max(0.0);
            let mut r = 2.0 * (2.0f32).powf(-ediff);
            if lm == MAX_LM {
                r *= std::f32::consts::SQRT_2;
            }
            r = r.min(thresh) * sqrt_1;

            let mask_idx = i * coded_channels + c;
            if mask_idx >= collapse_masks.len() {
                continue;
            }
            let collapse_mask = collapse_masks[mask_idx] & block_mask_limit;
            let mut injected_blocks = 0usize;
            let band = &mut chan[band_start..band_start + band_len];
            for k in 0..blocks {
                if (collapse_mask & (1u8 << k)) == 0 {
                    for j in 0..n0 {
                        seed_local = celt_lcg_rand(seed_local);
                        band[(j << lm) + k] = if (seed_local & LCG_SIGN_BIT) != 0 {
                            r
                        } else {
                            -r
                        };
                    }
                    injected_blocks += 1;
                }
            }
            if injected_blocks > 0 {
                vq::renormalise_vector(band, 1.0);
            }
        }
    }
}

/// Raíz cuadrada entera por defecto, `floor(sqrt(v))`, por el método de Newton.
fn isqrt32(v: u32) -> u32 {
    if v == 0 {
        return 0;
    }
    let mut x = v;
    let mut y = (x + 1) >> 1;
    while y < x {
        x = y;
        y = (x + v / x) >> 1;
    }
    x
}

/// Producto en Q15 de dos valores de 16 bits, redondeado (`FRAC_MUL16`).
fn frac_mul16(a: i32, b: i32) -> i32 {
    (Q15_ROUND + ((a as i16 as i32) * (b as i16 as i32))) >> Q15_SHIFT
}

/// Coseno exacto bit a bit de libopus (`bitexact_cos`): el ángulo va en Q14, de 0 a un cuarto de
/// vuelta (16384), y el resultado, en Q15 (hasta `Q15_ONE + 1`).
fn bitexact_cos(x: i32) -> i32 {
    let tmp = (COS_SQUARE_ROUND + x * x) >> COS_SQUARE_SHIFT;
    let x2 = tmp;
    let [c1, c2, c3] = BITEXACT_COS_COEFS;
    1 + (Q15_ONE - x2) + frac_mul16(x2, c1 + frac_mul16(x2, c2 + frac_mul16(c3, x2)))
}

/// `log2(isin / icos)` exacto bit a bit (`bitexact_log2tan`), en Q11: con él se reparte el
/// presupuesto entre las dos mitades de una división según su ángulo.
fn bitexact_log2tan(isin: i32, icos: i32) -> i32 {
    let lc = ec_ilog(icos.max(1) as u32);
    let ls = ec_ilog(isin.max(1) as u32);
    let icos_n = icos << (Q15_SHIFT - lc);
    let isin_n = isin << (Q15_SHIFT - ls);
    let [c1, c2] = BITEXACT_LOG2TAN_COEFS;
    (ls - lc) * (1 << LOG2TAN_Q) + frac_mul16(isin_n, frac_mul16(isin_n, c1) + c2)
        - frac_mul16(icos_n, frac_mul16(icos_n, c1) + c2)
}

/// Una etapa de la transformada de Haar de la reorganización tiempo-frecuencia (`haar1`): cada
/// par de coeficientes de `stride` en `stride` pasa a su suma y su diferencia, por `1/√2`.
fn haar1(x: &mut [f32], n0: usize, stride: usize) {
    let half = n0 >> 1;
    for i in 0..stride {
        for j in 0..half {
            let idx0 = stride * 2 * j + i;
            let idx1 = stride * (2 * j + 1) + i;
            let tmp1 = std::f32::consts::FRAC_1_SQRT_2 * x[idx0];
            let tmp2 = std::f32::consts::FRAC_1_SQRT_2 * x[idx1];
            x[idx0] = tmp1 + tmp2;
            x[idx1] = tmp1 - tmp2;
        }
    }
}

/// Desentrelaza `stride` bloques de `n0` coeficientes (`deinterleave_hadamard`); con
/// `hadamard`, en el orden de `ORDERY_TABLE`.
fn deinterleave_hadamard(x: &mut [f32], n0: usize, stride: usize, hadamard: bool) {
    let n = n0 * stride;
    let mut tmp = vec![0.0f32; n];
    if hadamard {
        let ordery = &ORDERY_TABLE[(stride - 2)..];
        for i in 0..stride {
            for j in 0..n0 {
                tmp[ordery[i] * n0 + j] = x[j * stride + i];
            }
        }
    } else {
        for i in 0..stride {
            for j in 0..n0 {
                tmp[i * n0 + j] = x[j * stride + i];
            }
        }
    }
    x[..n].copy_from_slice(&tmp);
}

/// Inversa de [`deinterleave_hadamard`] (`interleave_hadamard`).
fn interleave_hadamard(x: &mut [f32], n0: usize, stride: usize, hadamard: bool) {
    let n = n0 * stride;
    let mut tmp = vec![0.0f32; n];
    if hadamard {
        let ordery = &ORDERY_TABLE[(stride - 2)..];
        for i in 0..stride {
            for j in 0..n0 {
                tmp[j * stride + i] = x[ordery[i] * n0 + j];
            }
        }
    } else {
        for i in 0..stride {
            for j in 0..n0 {
                tmp[j * stride + i] = x[i * n0 + j];
            }
        }
    }
    x[..n].copy_from_slice(&tmp);
}

/// Deshace en la banda decodificada la reorganización tiempo-frecuencia de `quant_band`:
/// entrelazado, `time_divide` etapas de Haar y `recombine` recombinaciones.
fn apply_quant_band_post_tf(
    x: &mut [f32],
    n: usize,
    b0: usize,
    recombine: usize,
    time_divide: usize,
    long_blocks: bool,
) {
    let mut n_b = n / b0.max(1);
    if b0 > 1 {
        interleave_hadamard(x, n_b >> recombine, b0 << recombine, long_blocks);
    }
    let mut b = b0;
    for _ in 0..time_divide {
        b >>= 1;
        n_b <<= 1;
        haar1(x, n_b, b);
    }
    for k in 0..recombine {
        haar1(x, n >> k, 1 << k);
    }
}

/// Lleva la máscara de colapso de `quant_partition`, con `b0` bloques reorganizados, a los
/// bloques de la trama (final de `quant_band`).
fn post_tf_collapse_mask(mut cm: u32, b0: usize, recombine: usize, time_divide: usize) -> u32 {
    let mut b = b0.max(1);
    for _ in 0..time_divide {
        b >>= 1;
        if b > 0 {
            cm |= cm >> b;
        }
    }
    for _ in 0..recombine {
        cm = BIT_DEINTERLEAVE_TABLE[(cm & INTERLEAVE_NIBBLE_MASK) as usize] as u32;
    }
    let final_b = b << recombine;
    if final_b >= u32::BITS as usize {
        cm
    } else {
        cm & ((1u32 << final_b) - 1)
    }
}

/// Lleva la máscara de relleno de los bloques de la trama a los de la partición tras la
/// reorganización tiempo-frecuencia (principio de `quant_band`).
fn transform_fill_for_quant(
    mut fill: u32,
    mut blocks_orig: usize,
    recombine: usize,
    time_divide: usize,
) -> u32 {
    for _ in 0..recombine {
        fill = interleave_fill(fill);
    }
    for _ in 0..time_divide {
        if blocks_orig == 0 {
            break;
        }
        fill |= fill << blocks_orig;
        blocks_orig <<= 1;
    }
    fill
}

/// Copia de la banda de plegado con la misma reorganización tiempo-frecuencia que la banda que
/// se decodifica (`quant_band`); si no tiene su longitud, se copia tal cual.
fn transform_lowband_for_decode(
    lowband: &[f32],
    n: usize,
    frame_blocks: usize,
    recombine: usize,
    tf_change: i32,
    long_blocks: bool,
) -> Vec<f32> {
    if lowband.len() != n {
        return lowband.to_vec();
    }
    let mut out = lowband.to_vec();
    let mut n_b = n / frame_blocks.max(1);
    for k in 0..recombine {
        haar1(&mut out, n >> k, 1 << k);
    }
    let mut b = frame_blocks >> recombine;
    n_b <<= recombine;
    let mut tf = tf_change;
    while (n_b & 1) == 0 && tf < 0 {
        haar1(&mut out, n_b, b);
        b <<= 1;
        n_b >>= 1;
        tf += 1;
    }
    if b > 1 {
        deinterleave_hadamard(&mut out, n_b >> recombine, b << recombine, long_blocks);
    }
    out
}

/// Plegado especial de la primera banda híbrida (`special_hybrid_folding`): duplica el final del
/// historial de la banda `start` para que la `start + 1` tenga de dónde plegar. El canal derecho
/// solo en estéreo dual.
fn special_hybrid_folding_decode(
    mode: &CeltMode,
    norm_x: &mut [f32],
    norm_y: &mut [f32],
    start: usize,
    m: usize,
    dual_stereo_on: bool,
) {
    if start + 2 >= mode.e_bands.len() {
        return;
    }
    let n1 = m * (mode.e_bands[start + 1] as usize - mode.e_bands[start] as usize);
    let n2 = m * (mode.e_bands[start + 2] as usize - mode.e_bands[start + 1] as usize);
    if n2 <= n1 || n1 < (n2 - n1) {
        return;
    }
    let len = n2 - n1;
    let src_start = 2 * n1 - n2;
    let src_end = src_start + len;
    let dst_start = n1;
    let dst_end = dst_start + len;
    if src_end <= norm_x.len() && dst_end <= norm_x.len() {
        norm_x.copy_within(src_start..src_end, dst_start);
    }
    if dual_stereo_on && src_end <= norm_y.len() && dst_end <= norm_y.len() {
        norm_y.copy_within(src_start..src_end, dst_start);
    }
}

/// Pasa de medio y lateral a izquierdo y derecho (`stereo_merge`): `x` es el medio sin escalar,
/// `y` el lateral ya escalado y `mid` la ganancia del medio. Cada canal sale con norma unidad;
/// si alguno no tiene energía, los dos quedan iguales al medio.
fn stereo_merge_float(x: &mut [f32], y: &mut [f32], mid: f32) {
    let xp = x.iter().zip(y.iter()).map(|(a, b)| a * b).sum::<f32>() * mid;
    let side_energy = y.iter().map(|v| v * v).sum::<f32>();
    let el = mid * mid + side_energy - 2.0 * xp;
    let er = mid * mid + side_energy + 2.0 * xp;
    if er < STEREO_MERGE_MIN_ENERGY || el < STEREO_MERGE_MIN_ENERGY {
        y.copy_from_slice(x);
        return;
    }
    let lgain = 1.0 / el.sqrt();
    let rgain = 1.0 / er.sqrt();
    for i in 0..x.len() {
        let l = mid * x[i] - y[i];
        let r = mid * x[i] + y[i];
        x[i] = lgain * l;
        y[i] = rgain * r;
    }
}

/// Pasos con que se cuantifica el ángulo de una división (`compute_qn`), según el tamaño `n`, el
/// presupuesto `b`, el desplazamiento `offset` y el tope de pulsos `pulse_cap`: 1 si no hay
/// bits para más y, si no, un número par de como mucho `MAX_QN`.
fn compute_qn(n: usize, b: i32, offset: i32, pulse_cap: i32, stereo: bool) -> i32 {
    let mut n2 = 2 * n as i32 - 1;
    if stereo && n == 2 {
        n2 -= 1;
    }
    let mut qb = (b + n2 * offset) / n2;
    qb = qb.min(b - pulse_cap - (QN_RESERVED_BITS << BITRES));
    qb = qb.min(QN_MAX_BITS << BITRES);
    if qb < (1 << BITRES >> 1) {
        1
    } else {
        let mut qn =
            EXP2_TABLE8[(qb & ((1 << BITRES) - 1)) as usize] >> (EXP2_TABLE8_Q - (qb >> BITRES));
        qn = ((qn + 1) >> 1) << 1;
        qn.min(MAX_QN)
    }
}

/// Estado que comparten todas las particiones de una banda (la parte de `band_ctx` de libopus
/// que usa el decodificador): modo, banda, dispersión, decodificador de rango, semilla del ruido
/// de plegado y bits que le quedan a la banda, en octavos de bit.
struct PartitionDecoder<'a, 'e> {
    mode: &'a CeltMode,
    band: usize,
    spread: i32,
    dec: &'a mut EcDec<'e>,
    seed: &'a mut u32,
    remaining_bits: i32,
}

/// Lo que cambia entre una partición y sus mitades: presupuesto `b` en octavos de bit, bloques,
/// `lm` (baja uno en cada división), ganancia acumulada y máscara de relleno.
#[derive(Debug, Clone, Copy)]
struct PartitionSplit {
    b: i32,
    blocks: usize,
    lm: i32,
    gain: f32,
    fill: u32,
}

/// Parámetros estéreo de la trama: estéreo dual (canales codificados por separado), banda desde
/// la que se codifica en intensidad y prohibición de invertir la fase.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StereoParams {
    pub(crate) dual_stereo: bool,
    pub(crate) intensity: i32,
    pub(crate) disable_inv: bool,
}

/// Ángulo de una división mono (`compute_theta`): reparto de bits `delta`, bits gastados en
/// decodificarlo, ángulo en Q14 y coseno y seno en Q15.
struct MonoTheta {
    delta: i32,
    qalloc: i32,
    itheta: i32,
    imid: i32,
    iside: i32,
}

/// Ángulo de una banda estéreo: además de lo de [`MonoTheta`], la inversión de fase y las
/// ganancias de los canales medio y lateral.
struct StereoTheta {
    inv: bool,
    delta: i32,
    qalloc: i32,
    itheta: i32,
    mid_gain: f32,
    side_gain: f32,
}

/// Bits que pasan a la segunda mitad cuando la primera gastó menos de lo asignado
/// (`rebalance` de libopus): lo sobrante por encima de `REBALANCE_THRESHOLD_BITS`.
fn rebalance_bonus(allocated: i32, spent: i32) -> i32 {
    let rebalance = allocated - spent;
    if rebalance > (REBALANCE_THRESHOLD_BITS << BITRES) {
        rebalance - (REBALANCE_THRESHOLD_BITS << BITRES)
    } else {
        0
    }
}

impl PartitionDecoder<'_, '_> {
    /// Signo de un coeficiente aislado (`quant_band_n1`): un bit si queda presupuesto; si no,
    /// positivo.
    fn decode_sign(&mut self) -> f32 {
        if self.remaining_bits < (1 << BITRES) {
            return 1.0;
        }
        let sign = self.dec.dec_bits(1);
        self.remaining_bits -= 1 << BITRES;
        if sign != 0 { -1.0 } else { 1.0 }
    }

    /// Decodifica el ángulo de una división mono; `half` trae ya los bloques y `lm` de cada
    /// mitad, y se le descuentan los bits gastados y se recorta su máscara de relleno.
    fn decode_theta_mono(
        &mut self,
        n: usize,
        half: &mut PartitionSplit,
        blocks0: usize,
    ) -> MonoTheta {
        let pulse_cap = self.mode.log_n[self.band] as i32 + half.lm * (1 << BITRES);
        let offset = (pulse_cap >> 1) - QTHETA_OFFSET;
        let qn = compute_qn(n, half.b, offset, pulse_cap, false);
        let tell_before = self.dec.tell_frac() as i32;
        let mut itheta = 0i32;
        if qn != 1 {
            if blocks0 > 1 {
                itheta = self.dec.dec_uint((qn + 1) as u32) as i32;
            } else {
                itheta = self.decode_triangular_theta(qn);
            }
            itheta = (itheta * ITHETA_QUARTER_TURN) / qn;
        }
        let qalloc = self.dec.tell_frac() as i32 - tell_before;
        half.b -= qalloc;

        let blocks = half.blocks;
        if itheta == 0 {
            half.fill &= (1u32 << blocks) - 1;
            return MonoTheta {
                delta: -ITHETA_QUARTER_TURN,
                qalloc,
                itheta,
                imid: Q15_ONE,
                iside: 0,
            };
        }
        if itheta == ITHETA_QUARTER_TURN {
            half.fill &= ((1u32 << blocks) - 1) << blocks;
            return MonoTheta {
                delta: ITHETA_QUARTER_TURN,
                qalloc,
                itheta,
                imid: 0,
                iside: Q15_ONE,
            };
        }
        let imid = bitexact_cos(itheta);
        let iside = bitexact_cos(ITHETA_QUARTER_TURN - itheta);
        let delta = frac_mul16((n as i32 - 1) << 7, bitexact_log2tan(iside, imid));
        MonoTheta {
            delta,
            qalloc,
            itheta,
            imid,
            iside,
        }
    }

    /// Ángulo con distribución triangular (bloques largos), sin escalar a Q14.
    fn decode_triangular_theta(&mut self, qn: i32) -> i32 {
        let ft = ((qn >> 1) + 1) * ((qn >> 1) + 1);
        let fm = self.dec.decode(ft as u32) as i32;
        let (fl, fs, ith) = if fm < (((qn >> 1) * ((qn >> 1) + 1)) >> 1) {
            let ith = ((isqrt32((8 * fm + 1) as u32) as i32) - 1) >> 1;
            let fs = ith + 1;
            let fl = (ith * (ith + 1)) >> 1;
            (fl, fs, ith)
        } else {
            let ith = (2 * (qn + 1) - (isqrt32((8 * (ft - fm - 1) + 1) as u32) as i32)) >> 1;
            let fs = qn + 1 - ith;
            let fl = ft - (((qn + 1 - ith) * (qn + 2 - ith)) >> 1);
            (fl, fs, ith)
        };
        self.dec.update(fl as u32, (fl + fs) as u32, ft as u32);
        ith
    }

    /// Decodifica el ángulo estéreo de una banda de `n` coeficientes por canal; descuenta de `b`
    /// los bits gastados y recorta `fill`.
    fn decode_theta_stereo(
        &mut self,
        n: usize,
        b: &mut i32,
        blocks: usize,
        lm: i32,
        fill: &mut u32,
        stereo: StereoParams,
    ) -> StereoTheta {
        let pulse_cap = self.mode.log_n[self.band] as i32 + lm * (1 << BITRES);
        let offset = (pulse_cap >> 1)
            - if n == 2 {
                QTHETA_OFFSET_TWOPHASE
            } else {
                QTHETA_OFFSET
            };
        let mut qn = compute_qn(n, *b, offset, pulse_cap, true);
        if self.band as i32 >= stereo.intensity {
            qn = 1;
        }
        let tell_before = self.dec.tell_frac() as i32;
        let mut itheta: i32;
        let mut inv = false;
        if qn != 1 {
            if n > 2 {
                let p0 = THETA_STEREO_STEP_WEIGHT;
                let x0 = qn >> 1;
                let ft = p0 * (x0 + 1) + x0;
                let fs = self.dec.decode(ft as u32) as i32;
                let x = if fs < (x0 + 1) * p0 {
                    fs / p0
                } else {
                    x0 + 1 + (fs - (x0 + 1) * p0)
                };
                let (fl, fh) = if x <= x0 {
                    (p0 * x, p0 * (x + 1))
                } else {
                    ((x - 1 - x0) + (x0 + 1) * p0, (x - x0) + (x0 + 1) * p0)
                };
                self.dec.update(fl as u32, fh as u32, ft as u32);
                itheta = x;
            } else {
                itheta = self.dec.dec_uint((qn + 1) as u32) as i32;
            }
            itheta = (itheta * ITHETA_QUARTER_TURN) / qn;
        } else {
            if *b > (2 << BITRES) && self.remaining_bits > (2 << BITRES) {
                inv = self.dec.dec_bit_logp(2);
            }
            if stereo.disable_inv {
                inv = false;
            }
            itheta = 0;
        }
        let qalloc = self.dec.tell_frac() as i32 - tell_before;
        *b -= qalloc;
        if itheta == 0 {
            *fill &= (1u32 << blocks) - 1;
            return StereoTheta {
                inv,
                delta: -ITHETA_QUARTER_TURN,
                qalloc,
                itheta,
                mid_gain: 1.0,
                side_gain: 0.0,
            };
        }
        if itheta == ITHETA_QUARTER_TURN {
            *fill &= ((1u32 << blocks) - 1) << blocks;
            return StereoTheta {
                inv,
                delta: ITHETA_QUARTER_TURN,
                qalloc,
                itheta,
                mid_gain: 0.0,
                side_gain: 1.0,
            };
        }
        let imid = bitexact_cos(itheta);
        let iside = bitexact_cos(ITHETA_QUARTER_TURN - itheta);
        let delta = frac_mul16((n as i32 - 1) << 7, bitexact_log2tan(iside, imid));
        StereoTheta {
            inv,
            delta,
            qalloc,
            itheta,
            mid_gain: (imid as f32) * (1.0 / Q15_SCALE),
            side_gain: (iside as f32) * (1.0 / Q15_SCALE),
        }
    }

    /// Decodifica una banda estéreo codificada en medio y lateral (`quant_band` con dos canales,
    /// fuera del estéreo dual). Devuelve su máscara de colapso y el canal medio escalado, que es
    /// el historial de plegado de la banda.
    fn stereo_band(
        &mut self,
        band: StereoBand<'_>,
        mut b: i32,
        mut fill: u32,
        stereo: StereoParams,
    ) -> (u32, Vec<f32>) {
        let n = band.x.len();
        let mut mid_history = vec![0.0f32; n];
        if n == 1 {
            band.x[0] = self.decode_sign();
            band.y[0] = self.decode_sign();
            // Como `quant_band_n1` de libopus: X[0] queda como historial de plegado.
            mid_history[0] = band.x[0];
            return (1, mid_history);
        }
        let tf = band.tf;
        let orig_fill = fill;
        let theta =
            self.decode_theta_stereo(n, &mut b, tf.frame_blocks, band.lm, &mut fill, stereo);
        let cm = if n == 2 {
            self.stereo_band_n2(band, &theta, b, tf.quant_fill(orig_fill), &mut mid_history)
        } else {
            let fill_side = if tf.frame_blocks >= u32::BITS as usize {
                0
            } else {
                fill >> tf.frame_blocks
            };
            self.stereo_band_split(
                band,
                &theta,
                b,
                tf.quant_fill(fill),
                tf.quant_fill(fill_side),
                &mut mid_history,
            )
        };
        (cm, mid_history)
    }

    /// Banda estéreo de dos coeficientes: se decodifica un canal y el otro sale de girarlo un
    /// cuarto de vuelta, con un bit de signo si el ángulo no es 0 ni un cuarto de vuelta.
    fn stereo_band_n2(
        &mut self,
        band: StereoBand<'_>,
        theta: &StereoTheta,
        b: i32,
        fill: u32,
        mid_history: &mut [f32],
    ) -> u32 {
        let StereoBand {
            x,
            y,
            lowband_x,
            lm,
            tf,
        } = band;
        let sbits = if theta.itheta != 0 && theta.itheta != ITHETA_QUARTER_TURN {
            1 << BITRES
        } else {
            0
        };
        let mbits = b - sbits;
        self.remaining_bits -= theta.qalloc + sbits;
        let sign = if sbits != 0 {
            self.dec.dec_bits(1) as i32
        } else {
            0
        };
        let sign = (1 - 2 * sign) as f32;
        let (coded, rotated) = if theta.itheta > ITHETA_EIGHTH_TURN {
            (&mut *y, &mut *x)
        } else {
            (&mut *x, &mut *y)
        };
        let cm = self.partition(
            coded,
            lowband_x,
            PartitionSplit {
                b: mbits,
                blocks: tf.blocks,
                lm,
                gain: 1.0,
                fill,
            },
        );
        tf.post_process(coded);
        rotated[0] = -sign * coded[1];
        rotated[1] = sign * coded[0];
        store_scaled(mid_history, coded);
        let xl0 = theta.mid_gain * x[0];
        let xl1 = theta.mid_gain * x[1];
        let yr0 = theta.side_gain * y[0];
        let yr1 = theta.side_gain * y[1];
        x[0] = xl0 - yr0;
        y[0] = xl0 + yr0;
        x[1] = xl1 - yr1;
        y[1] = xl1 + yr1;
        if theta.inv {
            y[0] = -y[0];
            y[1] = -y[1];
        }
        tf.collapse_mask(cm)
    }

    /// Banda estéreo de más de dos coeficientes: medio y lateral como dos particiones mono; la
    /// que recibe más bits va primero y lo que no gaste pasa a la otra.
    fn stereo_band_split(
        &mut self,
        band: StereoBand<'_>,
        theta: &StereoTheta,
        b: i32,
        fill_mid: u32,
        fill_side: u32,
        mid_history: &mut [f32],
    ) -> u32 {
        let StereoBand {
            x,
            y,
            lowband_x,
            lm,
            tf,
        } = band;
        let mut mbits = 0.max(b.min((b - theta.delta) / 2));
        let mut sbits = b - mbits;
        self.remaining_bits -= theta.qalloc;
        let before = self.remaining_bits;
        let mid_split = |bits: i32| PartitionSplit {
            b: bits,
            blocks: tf.blocks,
            lm,
            gain: 1.0,
            fill: fill_mid,
        };
        let side_split = |bits: i32| PartitionSplit {
            b: bits,
            blocks: tf.blocks,
            lm,
            gain: theta.side_gain,
            fill: fill_side,
        };
        let cm = if mbits >= sbits {
            let mut cm = self.partition(x, lowband_x, mid_split(mbits));
            if theta.itheta != 0 {
                sbits += rebalance_bonus(mbits, before - self.remaining_bits);
            }
            cm |= self.partition(y, None, side_split(sbits));
            cm
        } else {
            let mut cm = self.partition(y, None, side_split(sbits));
            if theta.itheta != ITHETA_QUARTER_TURN {
                mbits += rebalance_bonus(sbits, before - self.remaining_bits);
            }
            cm |= self.partition(x, lowband_x, mid_split(mbits));
            cm
        };
        tf.post_process(x);
        tf.post_process(y);
        store_scaled(mid_history, x);
        stereo_merge_float(x, y, theta.mid_gain);
        if theta.inv {
            for v in y.iter_mut() {
                *v = -*v;
            }
        }
        tf.collapse_mask(cm)
    }

    /// Decodifica una partición mono, dividiéndola en dos mitades mientras el presupuesto lo
    /// justifique (`quant_partition`). Devuelve su máscara de colapso.
    fn partition(&mut self, x: &mut [f32], lowband: Option<&[f32]>, split: PartitionSplit) -> u32 {
        if x.len() == 1 {
            x[0] = self.decode_sign();
            return 1;
        }
        if self.should_split(x.len(), split) {
            return self.split_partition(x, lowband, split);
        }
        self.decode_leaf(x, lowband, split)
    }

    /// Si una partición de `n0` coeficientes con este presupuesto se divide: cuando `b` supera
    /// en más de `SPLIT_CACHE_MARGIN` los bits del mayor número de pulsos de la caché.
    fn should_split(&self, n0: usize, split: PartitionSplit) -> bool {
        let lm = split.lm;
        let cache_row = ((lm + 1) as usize)
            .saturating_mul(self.mode.nb_ebands)
            .saturating_add(self.band);
        if lm == -1 || n0 <= 2 || cache_row >= self.mode.cache.index.len() {
            return false;
        }
        let base = self.mode.cache.index[cache_row];
        if base < 0 {
            return false;
        }
        let cache = &self.mode.cache.bits[base as usize..];
        split.b > cache[cache[0] as usize] as i32 + SPLIT_CACHE_MARGIN
    }

    /// Divide la partición en dos mitades con su ángulo y decodifica primero la que recibe más
    /// bits; lo que esa no gaste pasa a la otra.
    fn split_partition(
        &mut self,
        x: &mut [f32],
        lowband: Option<&[f32]>,
        split: PartitionSplit,
    ) -> u32 {
        let n = x.len() >> 1;
        let blocks0 = split.blocks;
        let (x_lo, x_hi) = x.split_at_mut(n);
        let mut half = split;
        half.lm -= 1;
        if half.blocks == 1 {
            half.fill = (half.fill & 1) | (half.fill << 1);
        }
        half.blocks = (half.blocks + 1) >> 1;
        let theta = self.decode_theta_mono(n, &mut half, blocks0);
        let mid = (theta.imid as f32) * (1.0 / Q15_SCALE);
        let side = (theta.iside as f32) * (1.0 / Q15_SCALE);
        let mut delta = theta.delta;
        let lm = half.lm;
        if blocks0 > 1 && (theta.itheta & (ITHETA_QUARTER_TURN - 1)) != 0 {
            if theta.itheta > ITHETA_EIGHTH_TURN {
                delta -= delta >> (DELTA_DAMP_SHIFT_BASE - lm);
            } else {
                delta = 0.min(delta + (((n as i32) << BITRES) >> (DELTA_BIAS_SHIFT_BASE - lm)));
            }
        }
        let b = half.b;
        let mut mbits = 0.max(b.min((b - delta) / 2));
        let mut sbits = b - mbits;
        self.remaining_bits -= theta.qalloc;
        let low_lo = lowband.map(|lb| &lb[..lb.len().min(n)]);
        let low_hi = lowband.map(|lb| if lb.len() > n { &lb[n..] } else { &lb[0..0] });
        let mid_split = |bits: i32| PartitionSplit {
            b: bits,
            gain: split.gain * mid,
            ..half
        };
        let side_split = |bits: i32| PartitionSplit {
            b: bits,
            gain: split.gain * side,
            fill: half.fill >> half.blocks,
            ..half
        };
        let before = self.remaining_bits;
        if mbits >= sbits {
            let mut cm = self.partition(x_lo, low_lo, mid_split(mbits));
            if theta.itheta != 0 {
                sbits += rebalance_bonus(mbits, before - self.remaining_bits);
            }
            cm |= self.partition(x_hi, low_hi, side_split(sbits)) << (blocks0 >> 1);
            return cm;
        }
        let mut cm = self.partition(x_hi, low_hi, side_split(sbits)) << (blocks0 >> 1);
        if theta.itheta != ITHETA_QUARTER_TURN {
            mbits += rebalance_bonus(sbits, before - self.remaining_bits);
        }
        cm |= self.partition(x_lo, low_lo, mid_split(mbits));
        cm
    }

    /// Partición sin dividir: pulsos de la VQ si el presupuesto alcanza alguno; si no, plegado o
    /// ruido.
    fn decode_leaf(
        &mut self,
        x: &mut [f32],
        lowband: Option<&[f32]>,
        split: PartitionSplit,
    ) -> u32 {
        let PartitionSplit {
            b,
            blocks,
            lm,
            gain,
            fill,
        } = split;
        let mut q = rate::bits2pulses(self.mode, self.band, lm, b.max(0));
        let mut curr_bits = rate::pulses2bits(self.mode, self.band, lm, q);
        self.remaining_bits -= curr_bits;
        while self.remaining_bits < 0 && q > 0 {
            self.remaining_bits += curr_bits;
            q -= 1;
            curr_bits = rate::pulses2bits(self.mode, self.band, lm, q);
            self.remaining_bits -= curr_bits;
        }
        if q != 0 {
            let k = rate::get_pulses(q);
            return vq::alg_unquant(x, k, self.spread, blocks, self.dec, gain);
        }
        self.fill_unquantised(x, lowband, blocks, gain, fill)
    }

    /// Rellena una partición sin pulsos: a cero si su máscara de relleno está vacía; si no, con
    /// la banda de plegado más un ruido leve o, sin ella, con ruido pseudoaleatorio.
    fn fill_unquantised(
        &mut self,
        x: &mut [f32],
        lowband: Option<&[f32]>,
        blocks: usize,
        gain: f32,
        fill: u32,
    ) -> u32 {
        let n0 = x.len();
        let cm_mask = if blocks >= u32::BITS as usize {
            u32::MAX
        } else {
            (1u32 << blocks) - 1
        };
        let fill_masked = fill & cm_mask;
        if fill_masked == 0 {
            x.fill(0.0);
            return 0;
        }
        if let Some(lb) = lowband {
            if lb.is_empty() {
                for v in x.iter_mut() {
                    *self.seed = celt_lcg_rand(*self.seed);
                    *v = ((*self.seed as i32) >> NOISE_FILL_SHIFT) as f32;
                }
                vq::renormalise_vector(x, gain);
                return cm_mask;
            }
            for (j, v) in x.iter_mut().enumerate() {
                *self.seed = celt_lcg_rand(*self.seed);
                let noise = if (*self.seed & LCG_SIGN_BIT) != 0 {
                    FOLD_NOISE_LEVEL
                } else {
                    -FOLD_NOISE_LEVEL
                };
                *v = lb[j % lb.len()] + noise;
            }
            vq::renormalise_vector(x, gain);
            if blocks < u32::BITS as usize && fill_masked != cm_mask {
                let n_per_block = n0 / blocks.max(1);
                for b in 0..blocks {
                    if (fill_masked & (1u32 << b)) == 0 {
                        let start = b * n_per_block;
                        let end = (start + n_per_block).min(n0);
                        x[start..end].fill(0.0);
                    }
                }
            }
            return fill_masked;
        }
        for v in x.iter_mut() {
            *self.seed = celt_lcg_rand(*self.seed);
            *v = ((*self.seed as i32) >> NOISE_FILL_SHIFT) as f32;
        }
        vq::renormalise_vector(x, gain);
        cm_mask
    }
}

/// Datos de la trama que comparten todas sus bandas: los argumentos de `quant_all_bands` de
/// libopus que no cambian de una banda a otra.
pub(crate) struct BandsFrame<'a> {
    pub(crate) mode: &'a CeltMode,
    /// Bandas `start..end` que se decodifican.
    pub(crate) start: usize,
    pub(crate) end: usize,
    /// Bits asignados a cada banda, en octavos de bit.
    pub(crate) pulses: &'a [i32],
    pub(crate) short_blocks: bool,
    pub(crate) spread: i32,
    pub(crate) tf_res: &'a [i32],
    /// Presupuesto total de la trama, en octavos de bit.
    pub(crate) total_bits_q: i32,
    /// Sobrante inicial que se reparte entre las bandas.
    pub(crate) balance: i32,
    /// Bandas con bits asignados; las demás solo se pliegan.
    pub(crate) coded_bands: usize,
    pub(crate) lm: usize,
}

/// Reorganización tiempo-frecuencia de una banda según su `tf_change`: cuántas veces se
/// recombinan bloques (`recombine`) o se divide en el tiempo (`time_divide`), y los bloques que
/// ve la partición.
#[derive(Debug, Clone, Copy)]
struct BandTf {
    /// Bloques de la trama: `1 << lm` si es transitoria, 1 si no.
    frame_blocks: usize,
    /// Bloques tras la reorganización.
    blocks: usize,
    recombine: usize,
    time_divide: usize,
    long_blocks: bool,
    tf_change: i32,
    /// Si la banda de plegado necesita la misma reorganización antes de usarse.
    transforms_lowband: bool,
}

impl BandTf {
    /// Reorganización de una banda de `n` coeficientes.
    fn new(n: usize, frame_blocks: usize, tf_change: i32) -> Self {
        let recombine = tf_change.max(0) as usize;
        let n_b_initial = n / frame_blocks.max(1);
        let mut blocks = frame_blocks >> recombine;
        let mut n_b = n_b_initial << recombine;
        let mut tf = tf_change;
        let mut time_divide = 0usize;
        while (n_b & 1) == 0 && tf < 0 {
            blocks <<= 1;
            n_b >>= 1;
            tf += 1;
            time_divide += 1;
        }
        Self {
            frame_blocks,
            blocks,
            recombine,
            time_divide,
            long_blocks: frame_blocks == 1,
            tf_change,
            transforms_lowband: recombine > 0
                || ((n_b_initial & 1) == 0 && tf_change < 0)
                || frame_blocks > 1,
        }
    }

    /// Lleva una máscara de relleno de la trama a los bloques de la partición.
    fn fill(&self, mut fill: u32) -> u32 {
        for _ in 0..self.recombine {
            fill = interleave_fill(fill);
        }
        let mut blocks = self.frame_blocks >> self.recombine;
        for _ in 0..self.time_divide {
            if blocks < u32::BITS as usize {
                fill |= fill << blocks;
            } else {
                fill = u32::MAX;
            }
            blocks <<= 1;
        }
        fill
    }

    /// Deshace la reorganización sobre la banda ya decodificada.
    fn post_process(&self, x: &mut [f32]) {
        apply_quant_band_post_tf(
            x,
            x.len(),
            self.blocks,
            self.recombine,
            self.time_divide,
            self.long_blocks,
        );
    }

    /// Devuelve una máscara de colapso de la partición a los bloques de la trama.
    fn collapse_mask(&self, cm: u32) -> u32 {
        post_tf_collapse_mask(cm, self.blocks, self.recombine, self.time_divide)
    }

    /// Máscara de relleno de una partición de la banda estéreo (`transform_fill_for_quant`).
    fn quant_fill(&self, fill: u32) -> u32 {
        transform_fill_for_quant(fill, self.frame_blocks, self.recombine, self.time_divide)
    }

    /// Banda de plegado con la misma reorganización que la banda, si la necesita.
    fn lowband<'h>(&self, lowband: &'h [f32], n: usize) -> Cow<'h, [f32]> {
        if !self.transforms_lowband {
            return Cow::Borrowed(lowband);
        }
        Cow::Owned(transform_lowband_for_decode(
            lowband,
            n,
            self.frame_blocks,
            self.recombine,
            self.tf_change,
            self.long_blocks,
        ))
    }
}

/// Una banda estéreo a decodificar: sus dos canales, la banda de plegado del canal medio, `lm`
/// de la trama y su reorganización tiempo-frecuencia.
struct StereoBand<'x> {
    x: &'x mut [f32],
    y: &'x mut [f32],
    lowband_x: Option<&'x [f32]>,
    lm: i32,
    tf: BandTf,
}

/// Copia `src` en `dst` multiplicado por la raíz de su longitud (el historial de plegado guarda
/// las bandas con norma igual a su número de coeficientes).
fn store_scaled(dst: &mut [f32], src: &[f32]) {
    let scale = (src.len() as f32).sqrt();
    for (d, &s) in dst.iter_mut().zip(src) {
        *d = s * scale;
    }
}

/// Un paso de recombinación de una máscara de relleno (`bit_interleave_table` de libopus).
fn interleave_fill(fill: u32) -> u32 {
    let lo = (fill & INTERLEAVE_NIBBLE_MASK) as usize;
    let hi = ((fill >> INTERLEAVE_NIBBLE_BITS) & INTERLEAVE_NIBBLE_MASK) as usize;
    BIT_INTERLEAVE_TABLE[lo] | (BIT_INTERLEAVE_TABLE[hi] << 2)
}

/// Máscara con un bit por bloque, todos a uno.
fn all_blocks_mask(blocks: usize) -> u32 {
    if blocks >= u32::BITS as usize {
        u32::MAX
    } else {
        (1u32 << blocks) - 1
    }
}

/// Una banda lista para decodificar.
struct BandSlot {
    band: usize,
    /// Primer coeficiente de la banda y su número de coeficientes.
    band_start: usize,
    n: usize,
    /// Posición del decodificador de rango al empezar la banda, en octavos de bit.
    tell: i32,
    /// Bits de la banda y bits que le quedan a la trama, en octavos de bit.
    b: i32,
    remaining_bits: i32,
    tf: BandTf,
    /// Si se pliega: desplazamiento de la banda de plegado respecto al principio del historial
    /// normalizado y bandas cuyas máscaras de colapso hereda.
    fold: Option<(usize, Range<usize>)>,
}

impl BandSlot {
    /// Máscara de colapso heredada de las bandas de plegado, leídas con `mask_of`; sin plegado,
    /// todos los bloques.
    fn fold_mask(&self, mask_of: impl Fn(usize) -> u32) -> u32 {
        match &self.fold {
            Some((_, bands)) => bands.clone().fold(0, |cm, band| cm | mask_of(band)),
            None => all_blocks_mask(self.tf.frame_blocks),
        }
    }

    /// Reparto inicial de una partición de la banda con `b` bits y máscara de relleno `fill`.
    fn split(&self, b: i32, fill: u32, lm: usize) -> PartitionSplit {
        PartitionSplit {
            b,
            blocks: self.tf.blocks,
            lm: lm as i32,
            gain: 1.0,
            fill: self.tf.fill(fill),
        }
    }
}

/// Recorrido de las bandas de una trama: reparto del sobrante de bits (`balance`) y elección de
/// la banda de plegado (`lowband_offset`), iguales con uno y dos canales.
struct BandWalk<'f, 'a> {
    frame: &'f BandsFrame<'a>,
    /// Bloques cortos por trama (`1 << lm`).
    m: usize,
    /// Primer coeficiente de la banda `start`.
    norm_offset: usize,
    balance: i32,
    lowband_offset: usize,
    update_lowband: bool,
}

impl<'f, 'a> BandWalk<'f, 'a> {
    fn new(frame: &'f BandsFrame<'a>) -> Self {
        let m = 1usize << frame.lm;
        Self {
            frame,
            m,
            norm_offset: m * frame.mode.e_bands[frame.start] as usize,
            balance: frame.balance,
            lowband_offset: 0,
            update_lowband: true,
        }
    }

    /// Prepara la banda `i`: sus bits, su reorganización y su banda de plegado.
    fn begin(&mut self, i: usize, dec: &EcDec<'_>) -> BandSlot {
        let frame = self.frame;
        let e_bands = frame.mode.e_bands;
        let tell = dec.tell_frac() as i32;
        if i != frame.start {
            self.balance -= tell;
        }
        let remaining_bits = frame.total_bits_q - tell - 1;
        let b = if i < frame.coded_bands {
            let curr_balance =
                self.balance / ((frame.coded_bands - i).min(BALANCE_SPREAD_BANDS) as i32);
            (frame.pulses[i] + curr_balance)
                .clamp(0, MAX_BAND_BITS)
                .min(remaining_bits + 1)
        } else {
            0
        };
        let band_start = self.m * e_bands[i] as usize;
        let n = self.m * e_bands[i + 1] as usize - band_start;
        if i != frame.start && (self.update_lowband || self.lowband_offset == 0) {
            self.lowband_offset = i;
        }
        let frame_blocks = if frame.short_blocks { self.m } else { 1 };
        let tf_change = frame.tf_res[i];
        let folds = self.lowband_offset != 0
            && (frame.spread != SPREAD_AGGRESSIVE || frame_blocks > 1 || tf_change < 0);
        BandSlot {
            band: i,
            band_start,
            n,
            tell,
            b,
            remaining_bits,
            tf: BandTf::new(n, frame_blocks, tf_change),
            fold: folds.then(|| self.fold_source(i, n)),
        }
    }

    /// Banda de plegado de la banda `i`: desplazamiento respecto a `norm_offset` y bandas que
    /// abarca.
    fn fold_source(&self, i: usize, n: usize) -> (usize, Range<usize>) {
        let e_bands = self.frame.mode.e_bands;
        let lowband_start = self.m * e_bands[self.lowband_offset] as usize;
        let effective = lowband_start.saturating_sub(self.norm_offset + n);
        let fold_base = effective + self.norm_offset;
        let fold_limit = fold_base + n;
        let mut fold_start = self.lowband_offset;
        while fold_start > 0 {
            fold_start -= 1;
            if (self.m * e_bands[fold_start] as usize) <= fold_base {
                break;
            }
        }
        let mut fold_end = self.lowband_offset;
        while fold_end < i && (self.m * e_bands[fold_end] as usize) < fold_limit {
            fold_end += 1;
        }
        (effective, fold_start..fold_end)
    }

    /// Banda de plegado tomada del historial normalizado `history`, ya reorganizada.
    fn lowband<'h>(&self, slot: &BandSlot, history: &'h [f32]) -> Option<Cow<'h, [f32]>> {
        let (effective, _) = slot.fold.as_ref()?;
        let src_start = self.norm_offset + effective;
        let lowband = history.get(src_start..src_start + slot.n)?;
        Some(slot.tf.lowband(lowband, slot.n))
    }

    /// Decodificador de las particiones de la banda.
    fn partition_decoder<'d, 'e>(
        &self,
        slot: &BandSlot,
        dec: &'d mut EcDec<'e>,
        seed: &'d mut u32,
    ) -> PartitionDecoder<'d, 'e>
    where
        'a: 'd,
    {
        PartitionDecoder {
            mode: self.frame.mode,
            band: slot.band,
            spread: self.frame.spread.clamp(0, SPREAD_AGGRESSIVE),
            dec,
            seed,
            remaining_bits: slot.remaining_bits,
        }
    }

    /// Cierra la banda: devuelve al sobrante lo asignado y decide si la siguiente pliega desde
    /// ella.
    fn finish(&mut self, slot: &BandSlot) {
        self.balance += self.frame.pulses[slot.band] + slot.tell;
        self.update_lowband = slot.b > ((slot.n as i32) << BITRES);
    }

    /// Guarda la banda decodificada `band` en el historial normalizado, si no es la última.
    fn store_history(&self, slot: &BandSlot, history: &mut [f32], band: &[f32]) {
        if slot.band + 1 >= self.frame.end {
            return;
        }
        store_scaled(
            &mut history[slot.band_start..slot.band_start + slot.n],
            band,
        );
    }
}

/// Decodifica todas las bandas de una trama codificada en mono (`quant_all_bands` con un canal).
/// Escribe el espectro normalizado en `x` y devuelve la máscara de colapso de cada banda.
pub(crate) fn quant_all_bands_mono(
    frame: &BandsFrame<'_>,
    x: &mut [f32],
    dec: &mut EcDec<'_>,
    seed: &mut u32,
) -> Vec<u8> {
    let mut walk = BandWalk::new(frame);
    let mut masks = vec![0u8; frame.mode.nb_ebands];
    let mut norm_hist = vec![0.0f32; x.len()];
    for i in frame.start..frame.end {
        let slot = walk.begin(i, dec);
        // Como libopus, con cualquier número de canales: sin esto, en las tramas híbridas la
        // banda `start + 1` plegaría desde un historial que sigue a cero.
        if i == frame.start + 1 {
            let norm_offset = walk.norm_offset;
            special_hybrid_folding_decode(
                frame.mode,
                &mut norm_hist[norm_offset..],
                &mut [],
                frame.start,
                walk.m,
                false,
            );
        }
        let split = slot.split(slot.b, slot.fold_mask(|band| masks[band] as u32), frame.lm);
        let lowband = walk.lowband(&slot, &norm_hist);
        let band_x = &mut x[slot.band_start..slot.band_start + slot.n];
        let cm =
            walk.partition_decoder(&slot, dec, seed)
                .partition(band_x, lowband.as_deref(), split);
        slot.tf.post_process(band_x);
        let cm = slot.tf.collapse_mask(cm);
        walk.store_history(&slot, &mut norm_hist, band_x);
        masks[i] = (cm & u32::from(u8::MAX)) as u8;
        walk.finish(&slot);
    }
    masks
}

/// Decodifica todas las bandas de una trama codificada en estéreo (`quant_all_bands` con dos
/// canales). Escribe los espectros normalizados en `x` e `y` y devuelve la máscara de colapso de
/// cada canal y banda (`2 * nb_ebands`).
pub(crate) fn quant_all_bands_stereo(
    frame: &BandsFrame<'_>,
    x: &mut [f32],
    y: &mut [f32],
    dec: &mut EcDec<'_>,
    seed: &mut u32,
    stereo: StereoParams,
) -> Vec<u8> {
    let mode = frame.mode;
    let mut walk = BandWalk::new(frame);
    let norm_offset = walk.norm_offset;
    let mut masks = vec![0u8; mode.nb_ebands * 2];
    let mut dual_stereo_on = stereo.dual_stereo;
    let mut norm_hist_x = vec![0.0f32; x.len()];
    let mut norm_hist_y = vec![0.0f32; y.len()];

    for i in frame.start..frame.end {
        let slot = walk.begin(i, dec);
        if i == frame.start + 1 {
            special_hybrid_folding_decode(
                mode,
                &mut norm_hist_x[norm_offset..],
                &mut norm_hist_y[norm_offset..],
                frame.start,
                walk.m,
                dual_stereo_on,
            );
        }
        let x_cm = slot.fold_mask(|band| masks[band * 2] as u32);
        let y_cm = slot.fold_mask(|band| masks[band * 2 + 1] as u32);
        if dual_stereo_on && i == stereo.intensity as usize {
            dual_stereo_on = false;
            for j in norm_offset..slot.band_start {
                norm_hist_x[j] = 0.5 * (norm_hist_x[j] + norm_hist_y[j]);
            }
        }

        let band = slot.band_start..slot.band_start + slot.n;
        let band_x = &mut x[band.clone()];
        let band_y = &mut y[band];
        let (cm_l, cm_r) = if dual_stereo_on {
            let lowband_x = walk.lowband(&slot, &norm_hist_x);
            let lowband_y = walk.lowband(&slot, &norm_hist_y);
            let mut pd = walk.partition_decoder(&slot, dec, seed);
            let cmx = pd.partition(
                band_x,
                lowband_x.as_deref(),
                slot.split(slot.b / 2, x_cm, frame.lm),
            );
            let cmy = pd.partition(
                band_y,
                lowband_y.as_deref(),
                slot.split(slot.b / 2, y_cm, frame.lm),
            );
            slot.tf.post_process(band_x);
            slot.tf.post_process(band_y);
            walk.store_history(&slot, &mut norm_hist_x, band_x);
            walk.store_history(&slot, &mut norm_hist_y, band_y);
            (slot.tf.collapse_mask(cmx), slot.tf.collapse_mask(cmy))
        } else {
            let lowband_x = walk.lowband(&slot, &norm_hist_x);
            let (cm, mid_hist) = walk.partition_decoder(&slot, dec, seed).stereo_band(
                StereoBand {
                    x: band_x,
                    y: band_y,
                    lowband_x: lowband_x.as_deref(),
                    lm: frame.lm as i32,
                    tf: slot.tf,
                },
                slot.b,
                x_cm | y_cm,
                stereo,
            );
            if i + 1 < frame.end {
                norm_hist_x[slot.band_start..slot.band_start + slot.n].copy_from_slice(&mid_hist);
            }
            (cm, cm)
        };

        masks[i * 2] = (cm_l & u32::from(u8::MAX)) as u8;
        masks[i * 2 + 1] = (cm_r & u32::from(u8::MAX)) as u8;
        walk.finish(&slot);
    }
    masks
}

/// Desnormaliza las bandas `bands` del espectro `x` en `freq` con la energía decodificada
/// `band_loge` (log2) de cada una (`denormalise_bands`). Lo que queda fuera de `bands` no se toca.
pub(crate) fn denormalise_bands(
    mode: &CeltMode,
    x: &[f32],
    freq: &mut [f32],
    band_loge: &[f32],
    bands: Range<usize>,
    lm: usize,
) {
    let m = 1usize << lm;
    for (i, &loge) in bands.clone().zip(&band_loge[bands]) {
        let j0 = m * mode.e_bands[i] as usize;
        let j1 = m * mode.e_bands[i + 1] as usize;
        let lg = loge + quant_bands::e_means()[i];
        let g = 2.0f32.powf(lg.min(DENORMALISE_MAX_LOG_GAIN));
        for j in j0..j1 {
            freq[j] = x[j] * g;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::denormalise_bands;
    use crate::celt::modes::mode48000_960_120;
    use crate::celt::quant_bands::e_means;

    /// Logaritmo de ganancia muy por encima de la cota de libopus.
    const LG_HIGH: f32 = 100.0;
    /// Logaritmo de ganancia por debajo de −32, que libopus no limita.
    const LG_LOW: f32 = -40.0;

    /// Como `denormalise_bands` de libopus en coma flotante (`celt_exp2_db(MIN32(32.f, lg))`): la
    /// ganancia se limita a 2^32 por arriba y no se limita por abajo.
    #[test]
    fn denormalise_bounds_log_gain_like_libopus() {
        let mode = mode48000_960_120();
        let len = mode.e_bands[2] as usize;
        let x = vec![1.0f32; len];
        let mut freq = vec![0.0f32; len];
        let mut band_loge = vec![0.0f32; mode.nb_ebands];
        band_loge[0] = LG_HIGH - e_means()[0];
        band_loge[1] = LG_LOW - e_means()[1];

        denormalise_bands(mode, &x, &mut freq, &band_loge, 0..2, 0);

        let band0 = mode.e_bands[0] as usize..mode.e_bands[1] as usize;
        let band1 = mode.e_bands[1] as usize..len;
        assert!(freq[band0].iter().all(|&g| g == 2.0f32.powi(32)));
        assert!(freq[band1].iter().all(|&g| g == 2.0f32.powf(LG_LOW)));
    }
}
