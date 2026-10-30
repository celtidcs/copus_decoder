//! Emulación escalar, exacta bit a bit, de los núcleos x86 de libopus 1.6.1 en coma flotante que
//! usa la ocultación periódica de CELT.
//!
//! libopus para x86-64 da SSE por presente y elige AVX2 en tiempo de ejecución
//! (`celt/x86/pitch_sse.h`, `celt/x86/x86_celt_map.c`): `xcorr_kernel`, `celt_inner_prod` y
//! `comb_filter_const` van siempre por SSE, y `celt_pitch_xcorr` por AVX2 con FMA si la CPU lo
//! tiene. Cada núcleo suma en el orden de sus carriles, distinto del orden de la versión en C, y
//! en la búsqueda de tono un redondeo distinto puede cambiar el periodo elegido. Aquí se repite
//! ese orden carril a carril, sin SIMD, así que el resultado no depende de la CPU donde corra.

/// Carriles de un registro SSE de `f32`.
const SSE_LANES: usize = 4;
/// Carriles de un registro AVX de `f32`, y resultados que calcula a la vez `xcorr_kernel_avx`.
const AVX_LANES: usize = 8;

/// `xcorr_kernel_sse`: suma a `sum[k]` la correlación de `x[..len]` con `y[k..k + len]`, para
/// `k` en `0..4`. Alterna los términos entre dos acumuladores, que junta al final.
///
/// # Pánicos
///
/// Si `x` tiene menos de `len` muestras o `y` menos de `len + 3`.
pub(super) fn xcorr_kernel_sse(x: &[f32], y: &[f32], sum: &mut [f32; SSE_LANES], len: usize) {
    let mut even = *sum;
    let mut odd = [0.0f32; SSE_LANES];
    let mut j = 0usize;
    while j + 3 < len {
        for k in 0..SSE_LANES {
            even[k] += x[j] * y[j + k];
            odd[k] += x[j + 1] * y[j + 1 + k];
            even[k] += x[j + 2] * y[j + 2 + k];
            odd[k] += x[j + 3] * y[j + 3 + k];
        }
        j += SSE_LANES;
    }
    // La cola alterna igual: primero el acumulador par, después el impar y otra vez el par.
    for (step, jj) in (j..len).enumerate() {
        let acc = if step == 1 { &mut odd } else { &mut even };
        for k in 0..SSE_LANES {
            acc[k] += x[jj] * y[jj + k];
        }
    }
    for k in 0..SSE_LANES {
        sum[k] = even[k] + odd[k];
    }
}

/// `celt_inner_prod_sse`: producto escalar de `x[..n]` e `y[..n]`, con cuatro sumas parciales
/// juntadas como `(s0 + s2) + (s1 + s3)` y el resto sumado en orden.
///
/// # Pánicos
///
/// Si `x` o `y` tienen menos de `n` muestras.
pub(super) fn inner_prod_sse(x: &[f32], y: &[f32], n: usize) -> f32 {
    let mut partial = [0.0f32; SSE_LANES];
    let mut i = 0usize;
    while i + 3 < n {
        for k in 0..SSE_LANES {
            partial[k] += x[i + k] * y[i + k];
        }
        i += SSE_LANES;
    }
    let mut xy = (partial[0] + partial[2]) + (partial[1] + partial[3]);
    for j in i..n {
        xy += x[j] * y[j];
    }
    xy
}

/// `xcorr_kernel_avx`: escribe en `sum[r]` la correlación de `x[..len]` con `y[r..r + len]`, para
/// `r` en `0..8`, con FMA en ocho carriles; las muestras que faltan para llenar el último bloque
/// cuentan como ceros (`_mm256_maskload_ps`). Cada resultado junta sus carriles como
/// `((a0 + a4) + (a1 + a5)) + ((a2 + a6) + (a3 + a7))`.
fn xcorr_kernel_avx(x: &[f32], y: &[f32], sum: &mut [f32], len: usize) {
    let mut acc = [[0.0f32; AVX_LANES]; AVX_LANES];
    let mut i = 0usize;
    while i < len {
        for (r, lanes) in acc.iter_mut().enumerate() {
            for (l, lane) in lanes.iter_mut().enumerate() {
                let (xv, yv) = if i + l < len {
                    (x[i + l], y[i + r + l])
                } else {
                    (0.0, 0.0)
                };
                *lane = xv.mul_add(yv, *lane);
            }
        }
        i += AVX_LANES;
    }
    for (r, a) in acc.iter().enumerate() {
        sum[r] = ((a[0] + a[4]) + (a[1] + a[5])) + ((a[2] + a[6]) + (a[3] + a[7]));
    }
}

/// `celt_pitch_xcorr_avx2`: `xcorr[i]` es la correlación de `x[..len]` con `y[i..i + len]` para
/// `i` en `0..max_pitch`, de ocho en ocho con [`xcorr_kernel_avx`] y el resto con
/// [`inner_prod_sse`].
///
/// # Pánicos
///
/// Si `xcorr` tiene menos de `max_pitch` valores, `x` menos de `len` o `y` menos de
/// `max_pitch - 1 + len`.
pub(super) fn pitch_xcorr_avx2(
    x: &[f32],
    y: &[f32],
    xcorr: &mut [f32],
    len: usize,
    max_pitch: usize,
) {
    let mut i = 0usize;
    while i + AVX_LANES - 1 < max_pitch {
        xcorr_kernel_avx(x, &y[i..], &mut xcorr[i..i + AVX_LANES], len);
        i += AVX_LANES;
    }
    for (j, value) in xcorr.iter_mut().enumerate().take(max_pitch).skip(i) {
        *value = inner_prod_sse(x, &y[j..], len);
    }
}

/// Una muestra de la parte de ganancia constante del filtro peine, con las sumas parciales de
/// `comb_filter_const_sse`: `(x + g0·centre) + (g1·near + g2·far)`, donde `centre` es la muestra
/// un periodo atrás, `near` la suma de sus vecinas a distancia 1 y `far` la de las de distancia 2.
pub(super) fn comb_filter_const_sample(
    x: f32,
    centre: f32,
    near: f32,
    far: f32,
    g: [f32; 3],
) -> f32 {
    (x + g[0] * centre) + (g[1] * near + g[2] * far)
}

/// Parte de ganancia constante del filtro peine (`comb_filter_const_sse`): devuelve
/// `x[start + i]` filtrado con el periodo `period` y las ganancias `g`, para `i` en `0..n`, sin
/// modificar `x`. Suma como libopus con sumas parciales:
/// `(x + g0·x[-T]) + (g1·(x[-T+1] + x[-T-1]) + g2·(x[-T+2] + x[-T-2]))`.
///
/// # Pánicos
///
/// Si `start < period + 2` o `x` no llega a `start + n`.
pub(super) fn comb_filter_const_sse(
    x: &[f32],
    start: usize,
    period: usize,
    n: usize,
    g: [f32; 3],
) -> Vec<f32> {
    (start..start + n)
        .map(|i| {
            let near = x[i - period + 1] + x[i - period - 1];
            let far = x[i - period + 2] + x[i - period - 2];
            comb_filter_const_sample(x[i], x[i - period], near, far, g)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{inner_prod_sse, pitch_xcorr_avx2, xcorr_kernel_sse};

    /// Señales con valores enteros pequeños: sus productos y sumas son exactos en `f32`, así que
    /// cualquier orden de suma da lo mismo y se comprueban los índices, no el redondeo.
    fn signal(len: usize, seed: i32) -> Vec<f32> {
        (0..len as i32)
            .map(|i| ((i * 7 + seed) % 11 - 5) as f32)
            .collect()
    }

    fn naive_xcorr(x: &[f32], y: &[f32], len: usize, lag: usize) -> f32 {
        (0..len).map(|j| x[j] * y[j + lag]).sum()
    }

    #[test]
    fn sse_kernel_correlates_four_lags() {
        let (x, y) = (signal(27, 1), signal(40, 3));
        for len in [24, 25, 26, 27] {
            let mut sum = [1.0f32; 4];
            xcorr_kernel_sse(&x, &y, &mut sum, len);
            for (k, value) in sum.iter().enumerate() {
                assert_eq!(
                    *value,
                    1.0 + naive_xcorr(&x, &y, len, k),
                    "len {len}, k {k}"
                );
            }
        }
    }

    #[test]
    fn sse_inner_product_is_the_dot_product() {
        let (x, y) = (signal(31, 2), signal(31, 5));
        for n in [28, 29, 31] {
            assert_eq!(inner_prod_sse(&x, &y, n), naive_xcorr(&x, &y, n, 0));
        }
    }

    #[test]
    fn avx2_pitch_xcorr_correlates_every_lag() {
        let (x, y) = (signal(29, 4), signal(29 + 21, 6));
        for len in [24, 29] {
            let mut xcorr = vec![0.0f32; 21];
            pitch_xcorr_avx2(&x, &y, &mut xcorr, len, 21);
            for (lag, value) in xcorr.iter().enumerate() {
                assert_eq!(
                    *value,
                    naive_xcorr(&x, &y, len, lag),
                    "len {len}, lag {lag}"
                );
            }
        }
    }
}
