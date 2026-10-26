//! Primitivas de coma fija de SILK: traducción de las macros de `silk/SigProc_FIX.h`,
//! `silk/macros.h` e `silk/Inlines.h` de libopus.
//!
//! Cada función reproduce el resultado de su macro bit a bit, incluido el desbordamiento con
//! vuelta (`wrapping_*`) allí donde libopus lo admite. Los nombres siguen los de libopus en
//! minúsculas y sin el prefijo `silk_`, para poder cotejar cada llamada con la fuente.

/// Multiplicador y sumando del generador congruencial de SILK (`silk_RAND`).
const RAND_MULTIPLIER: i32 = 196_314_165;
const RAND_INCREMENT: i32 = 907_633_515;

/// Siguiente estado del generador pseudoaleatorio de SILK (`silk_RAND`), con desbordamiento
/// con vuelta.
pub(super) fn silk_rand(seed: i32) -> i32 {
    seed.wrapping_mul(RAND_MULTIPLIER)
        .wrapping_add(RAND_INCREMENT)
}

/// Producto de las mitades bajas de 16 bits con signo (`silk_SMULBB`).
pub(super) fn smulbb(a32: i32, b32: i32) -> i32 {
    (a32 as i16 as i32) * (b32 as i16 as i32)
}

/// `a32` más el producto de las mitades bajas de 16 bits de `b32` y `c32` (`silk_SMLABB`).
pub(super) fn smlabb(a32: i32, b32: i32, c32: i32) -> i32 {
    a32 + (b32 as i16 as i32) * (c32 as i16 as i32)
}

/// `(a32 * (i16)b32) >> 16` sin pasar por 64 bits (`silk_SMULWB`).
pub(super) fn smulwb(a32: i32, b32: i32) -> i32 {
    let b16 = b32 as i16 as i32;
    let high = (a32 >> 16) * b16;
    let low = ((a32 & 0xFFFF) * b16) >> 16;
    high.wrapping_add(low)
}

/// `a32 + ((b32 * (i16)c32) >> 16)` (`silk_SMLAWB`).
pub(super) fn smlawb(a32: i32, b32: i32, c32: i32) -> i32 {
    a32.wrapping_add(smulwb(b32, c32))
}

/// `(a32 * b32) >> 16` con el producto en 64 bits (`silk_SMULWW`).
pub(super) fn smulww(a32: i32, b32: i32) -> i32 {
    ((a32 as i64 * b32 as i64) >> 16) as i32
}

/// `a32 + ((b32 * c32) >> 16)` (`silk_SMLAWW`).
pub(super) fn smlaww(a32: i32, b32: i32, c32: i32) -> i32 {
    a32.wrapping_add(smulww(b32, c32))
}

/// Palabra alta con signo del producto de 64 bits: `(a32 * b32) >> 32` (`silk_SMMUL`).
pub(super) fn smmul(a32: i32, b32: i32) -> i32 {
    ((a32 as i64 * b32 as i64) >> 32) as i32
}

/// Suma con saturación a 32 bits (`silk_ADD_SAT32`).
pub(super) fn add_sat32(a: i32, b: i32) -> i32 {
    a.saturating_add(b)
}

/// Resta con desbordamiento con vuelta (`silk_SUB32_ovflw`).
pub(super) fn sub32_ovflw(a: i32, b: i32) -> i32 {
    a.wrapping_sub(b)
}

/// Desplazamiento a la izquierda con desbordamiento con vuelta (`silk_LSHIFT_ovflw`).
pub(super) fn lshift_ovflw(value: i32, shift: usize) -> i32 {
    ((value as u32) << shift) as i32
}

/// Desplazamiento a la izquierda con saturación (`silk_LSHIFT_SAT32`): primero se limita
/// `value` a `[i32::MIN >> shift, i32::MAX >> shift]` y después se desplaza, así que el máximo
/// es `(i32::MAX >> shift) << shift`, no `i32::MAX`. Con `shift >= 31` solo queda el signo.
pub(super) fn lshift_sat32(value: i32, shift: usize) -> i32 {
    if shift >= 31 {
        if value > 0 {
            i32::MAX
        } else if value < 0 {
            i32::MIN
        } else {
            0
        }
    } else {
        value
            .clamp(i32::MIN >> shift, i32::MAX >> shift)
            .wrapping_shl(shift as u32)
    }
}

/// Desplazamiento aritmético a la derecha con redondeo (`silk_RSHIFT_ROUND`). `shift` debe ser
/// al menos 1.
pub(super) fn rshift_round(value: i32, shift: usize) -> i32 {
    if shift == 1 {
        (value >> 1) + (value & 1)
    } else {
        ((value >> (shift - 1)) + 1) >> 1
    }
}

/// `rshift_round` sobre un valor de 64 bits, con el resultado truncado a 32
/// (`silk_RSHIFT_ROUND64`).
pub(super) fn rshift_round64(value: i64, shift: usize) -> i32 {
    if shift == 1 {
        ((value >> 1) + (value & 1)) as i32
    } else {
        (((value >> (shift - 1)) + 1) >> 1) as i32
    }
}

/// Satura a 16 bits con signo (`silk_SAT16`).
pub(super) fn sat16(value: i32) -> i16 {
    value.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

/// Valor absoluto para contar ceros a la izquierda: `i32::MIN` satura a `i32::MAX` en vez de
/// desbordar, como el `silk_abs` que precede a `silk_CLZ32` en `silk_DIV32_varQ`.
pub(super) fn abs32_for_clz(value: i32) -> i32 {
    if value == i32::MIN {
        i32::MAX
    } else {
        value.abs()
    }
}

/// Aproxima `(a32 << q_res) / b32` con un recíproco refinado (`silk_DIV32_varQ`). `b32` no
/// puede ser 0.
pub(super) fn div32_var_q(a32: i32, b32: i32, q_res: usize) -> i32 {
    debug_assert!(b32 != 0);
    if a32 == 0 {
        return 0;
    }

    let a_headrm = abs32_for_clz(a32).leading_zeros() as usize - 1;
    let mut a32_nrm = a32 << a_headrm;
    let b_headrm = abs32_for_clz(b32).leading_zeros() as usize - 1;
    let b32_nrm = b32 << b_headrm;

    let b32_inv = (i32::MAX >> 2) / (b32_nrm >> 16);
    let mut result = smulwb(a32_nrm, b32_inv);
    a32_nrm = sub32_ovflw(a32_nrm, lshift_ovflw(smmul(b32_nrm, result), 3));
    result = smlawb(result, a32_nrm, b32_inv);

    let lshift = 29isize + a_headrm as isize - b_headrm as isize - q_res as isize;
    if lshift < 0 {
        lshift_sat32(result, (-lshift) as usize)
    } else if lshift < 32 {
        result >> lshift
    } else {
        0
    }
}

/// Aproxima `(1 << q_res) / value` con un recíproco refinado (`silk_INVERSE32_varQ`). `value`
/// no puede ser 0 y `q_res` debe ser positivo.
pub(super) fn inverse32_var_q(value: i32, q_res: usize) -> i32 {
    debug_assert!(value != 0);
    debug_assert!(q_res > 0);

    let b_headrm = abs32_for_clz(value).leading_zeros() as usize - 1;
    let b32_nrm = value << b_headrm;
    let b32_inv = (i32::MAX >> 2) / (b32_nrm >> 16);
    let mut result = b32_inv << 16;
    let err_q32 = ((1i32 << 29) - smulwb(b32_nrm, b32_inv)) << 3;
    result = smlaww(result, err_q32, b32_inv);

    let lshift = 61isize - b_headrm as isize - q_res as isize;
    if lshift <= 0 {
        lshift_sat32(result, (-lshift) as usize)
    } else if lshift < 32 {
        result >> lshift
    } else {
        0
    }
}
