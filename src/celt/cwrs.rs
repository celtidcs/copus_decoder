//! Decodificación combinatoria de los pulsos PVQ de CELT (`cwrs.c`).

use crate::entropy::EcDec;

/// Fila de `U(n, i)` para `i` hasta `k + 1` y número de vectores `V(n, k)` con `k` pulsos en
/// `n` dimensiones. Devuelve `(fila, V(n, k))`, con `fila[i] = U(n, i)`.
fn ncwrs_urow(n: usize, k: usize) -> (Vec<u32>, u32) {
    let len = k + 2;
    let mut u = vec![0u32; len];
    u[0] = 0;
    u[1] = 1;
    for (idx, item) in u.iter_mut().enumerate().skip(2) {
        *item = ((idx as u32) << 1) - 1;
    }
    for _ in 2..n {
        unext(&mut u[1..], 1);
    }
    let v = u[k].wrapping_add(u[k + 1]);
    (u, v)
}

/// Avanza la fila de `U` una dimensión (`unext`), con `u0` como primer valor.
fn unext(u: &mut [u32], mut u0: u32) {
    if u.len() < 2 {
        return;
    }
    for j in 1..u.len() {
        let u1 = u[j].wrapping_add(u[j - 1]).wrapping_add(u0);
        u[j - 1] = u0;
        u0 = u1;
    }
    let last = u.len() - 1;
    u[last] = u0;
}

/// Retrocede la fila de `U` una dimensión (`uprev`), con `u0` como primer valor.
fn uprev(u: &mut [u32], mut u0: u32) {
    if u.len() < 2 {
        return;
    }
    for j in 1..u.len() {
        let u1 = u[j].wrapping_sub(u[j - 1]).wrapping_sub(u0);
        u[j - 1] = u0;
        u0 = u1;
    }
    let last = u.len() - 1;
    u[last] = u0;
}

/// Convierte el índice `idx` en el vector de `k` pulsos con signo y `n` dimensiones (`cwrsi`),
/// usando y gastando la fila de `U` de `u`.
fn cwrsi(n: usize, mut k: usize, mut idx: u32, u: &mut [u32]) -> Vec<i32> {
    let mut y = vec![0i32; n];
    for yj in y.iter_mut().take(n) {
        let p = u[k + 1];
        let neg = idx >= p;
        if neg {
            idx = idx.wrapping_sub(p);
        }
        let yk = k;
        let mut cur = u[k];
        while cur > idx {
            k -= 1;
            cur = u[k];
        }
        idx = idx.wrapping_sub(cur);
        let mut val = (yk - k) as i32;
        if neg {
            val = -val;
        }
        *yj = val;
        uprev(&mut u[..=k + 1], 0);
    }
    y
}

/// Lee del decodificador de rango un vector de `k` pulsos con signo y `n` dimensiones
/// (`decode_pulses`): un índice uniforme entre los `V(n, k)` posibles.
pub(crate) fn decode_pulses(dec: &mut EcDec<'_>, n: usize, k: usize) -> Vec<i32> {
    if n == 0 || k == 0 {
        return vec![0; n];
    }
    let (mut u, nc) = ncwrs_urow(n, k);
    let idx = dec.dec_uint(nc.max(2));
    cwrsi(n, k, idx, &mut u)
}
