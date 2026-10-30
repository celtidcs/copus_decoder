//! Decodificación de los vectores PVQ de CELT y su rotación de dispersión (`vq.c`).

use crate::celt::cwrs;
use crate::celt::x86_kernels::inner_prod_sse;
use crate::entropy::EcDec;

/// Modo de dispersión de CELT sin rotación (`SPREAD_NONE`).
pub(crate) const SPREAD_NONE: i32 = 0;
/// La rotación de segundo nivel solo se aplica a bandas de al menos 8 bloques del paso
/// (`len>=8*stride` en `exp_rotation`).
const ROTATION_STRIDE2_MIN_BLOCKS: usize = 8;
/// Energía que libopus suma al renormalizar para no dividir por cero (`EPSILON` en coma
/// flotante, `arch.h`).
const RENORMALISE_EPSILON: f32 = 1e-15;

/// Una etapa de la rotación exponencial (`exp_rotation1`): gira cada par de coeficientes
/// separados `stride` con el coseno `c` y el seno `s`, hacia delante y después hacia atrás.
fn exp_rotation1(x: &mut [f32], stride: usize, c: f32, s: f32) {
    let len = x.len();
    if len <= stride {
        return;
    }
    for i in 0..(len - stride) {
        let x1 = x[i];
        let x2 = x[i + stride];
        x[i + stride] = c * x2 + s * x1;
        x[i] = c * x1 - s * x2;
    }
    if len <= 2 * stride {
        return;
    }
    for i in (0..=(len - 2 * stride - 1)).rev() {
        let x1 = x[i];
        let x2 = x[i + stride];
        x[i + stride] = c * x2 + s * x1;
        x[i] = c * x1 - s * x2;
    }
}

/// Coseno de `½π·x` como `celt_cos_norm` de libopus en coma flotante (`celt/mathops.h`): calculado
/// en `f64` (allí `PI` es un `double`) y convertido a `f32`. Calcularlo en `f32` cambia el último
/// bit de la rotación y, por ella, la salida.
fn celt_cos_norm(x: f32) -> f32 {
    (core::f64::consts::FRAC_PI_2 * f64::from(x)).cos() as f32
}

/// Rotación de dispersión de CELT (`exp_rotation`): reparte la energía de una banda con pocos
/// pulsos entre sus coeficientes. El ángulo depende de los pulsos `k` y del modo de dispersión;
/// `dir` elige el sentido.
fn exp_rotation(x: &mut [f32], dir: i32, stride: usize, k: i32, spread: i32) {
    const SPREAD_FACTOR: [i32; 3] = [15, 10, 5];
    if 2 * k >= x.len() as i32 || spread == SPREAD_NONE {
        return;
    }
    let factor = SPREAD_FACTOR[(spread - 1) as usize];
    let gain = x.len() as f32 / (x.len() as f32 + factor as f32 * k as f32);
    let theta = 0.5 * gain * gain;
    let c = celt_cos_norm(theta);
    // Seno de ½π·θ como coseno del complementario, con `1 − θ` en `f32`, como libopus.
    let s = celt_cos_norm(1.0 - theta);
    let mut stride2 = 0usize;
    if x.len() >= ROTATION_STRIDE2_MIN_BLOCKS * stride {
        stride2 = 1;
        while (stride2 * stride2 + stride2) * stride + (stride >> 2) < x.len() {
            stride2 += 1;
        }
    }
    let len = x.len() / stride;
    for i in 0..stride {
        let xs = &mut x[i * len..(i + 1) * len];
        if dir < 0 {
            if stride2 > 0 {
                exp_rotation1(xs, stride2, s, c);
            }
            exp_rotation1(xs, 1, c, s);
        } else {
            exp_rotation1(xs, 1, c, -s);
            if stride2 > 0 {
                exp_rotation1(xs, stride2, s, -c);
            }
        }
    }
}

/// Escribe en el vector de salida los pulsos con norma `gain` y devuelve la suma de sus
/// cuadrados.
fn normalise_residual(iy: &[i32], x: &mut [f32], gain: f32) -> u32 {
    let ryy = iy
        .iter()
        .fold(0u32, |acc, &v| acc.wrapping_add((v * v) as u32));
    let scale = if ryy == 0 {
        0.0
    } else {
        // Como libopus: el inverso de la raíz por la ganancia, no la ganancia entre la raíz.
        (1.0 / (ryy as f32).sqrt()) * gain
    };
    for (dst, &src) in x.iter_mut().zip(iy.iter()) {
        *dst = src as f32 * scale;
    }
    ryy
}

/// Máscara de colapso para el anticolapso de las tramas transitorias: un bit por bloque corto,
/// a uno si el bloque tiene algún pulso.
///
/// Como en libopus (`extract_collapse_mask`, `unsigned`), la máscara es de 32 bits: con
/// LM=3 y `tf_change` negativo hay hasta 16 bloques, y los bits altos se pliegan después en
/// `post_tf_collapse_mask`. Con 8 bits, el desplazamiento desbordaba a partir del noveno bloque.
fn extract_collapse_mask(iy: &[i32], n: usize, b: usize) -> u32 {
    if b <= 1 {
        return 1;
    }
    let n0 = n / b;
    let mut mask = 0u32;
    for i in 0..b {
        let mut nonzero = 0i32;
        for j in 0..n0 {
            nonzero |= iy[i * n0 + j];
        }
        if nonzero != 0 {
            mask |= 1 << i;
        }
    }
    mask
}

/// Decodifica una banda PVQ con `k` pulsos (`alg_unquant`): pulsos, norma `gain` y rotación de
/// dispersión. Devuelve la máscara de colapso de sus `b` bloques.
pub(crate) fn alg_unquant(
    x: &mut [f32],
    k: i32,
    spread: i32,
    b: usize,
    dec: &mut EcDec<'_>,
    gain: f32,
) -> u32 {
    let pulses = cwrs::decode_pulses(dec, x.len(), k as usize);
    normalise_residual(&pulses, x, gain);
    exp_rotation(x, -1, b.max(1), k, spread);
    extract_collapse_mask(&pulses, x.len(), b.max(1))
}

/// Escala el vector para que su norma sea `gain` (`renormalise_vector`), con la energía sumada
/// como `celt_inner_prod_sse`, que es el producto escalar que usa libopus x86.
pub(crate) fn renormalise_vector(x: &mut [f32], gain: f32) {
    let e = RENORMALISE_EPSILON + inner_prod_sse(x, x, x.len());
    let g = (1.0 / e.sqrt()) * gain;
    for v in x.iter_mut() {
        *v *= g;
    }
}
