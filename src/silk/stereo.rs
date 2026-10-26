//! Estéreo de SILK: lectura de la predicción y del indicador de solo medio, y conversión de
//! medio y lateral a izquierdo y derecho.

use super::fixed::{rshift_round, sat16, smlawb, smulbb, smulww};
use crate::entropy::EcDec;

use super::tables;

const STEREO_STEP_Q16: i32 = 6_554; // SILK_FIX_CONST(0.5 / 5, 16)
const STEREO_INTERP_LEN_MS: usize = 8;
/// El símbolo conjunto codifica el grupo de cuantificación de los dos predictores, 5 × 5
/// (`silk_stereo_decode_pred`).
const STEREO_PRED_GROUPS: i32 = 5;
/// Intervalos de `STEREO_PRED_QUANT_Q13` por grupo: los elige `UNIFORM3_ICDF`
/// (`silk_stereo_decode_pred`).
const STEREO_PRED_INTERVALS_PER_GROUP: i32 = 3;

/// Estado estéreo de SILK del decodificador: los predictores de la trama anterior, en Q13, y las
/// dos últimas muestras de medio y de lateral, para la conversión a izquierdo y derecho.
#[derive(Debug, Clone, Default)]
pub(super) struct StereoState {
    pub pred_prev_q13: [i16; 2],
    pub s_mid: [i16; 2],
    pub s_side: [i16; 2],
}

impl StereoState {
    /// Prepara el estado para la primera trama estéreo tras una mono: olvida los predictores y la
    /// historia del lateral, que la trama mono no ha mantenido, y conserva la del medio, que sí
    /// (`silk_Decode` en `dec_API.c` de libopus).
    pub(super) fn reset_mono_to_stereo(&mut self) {
        self.pred_prev_q13 = [0; 2];
        self.s_side = [0; 2];
    }
}

/// Lee con el decodificador de rango compartido `dec` los dos predictores estéreo de la trama
/// (`silk_stereo_decode_pred`) y los devuelve en Q13.
pub(super) fn decode_stereo_pred(dec: &mut EcDec<'_>) -> [i32; 2] {
    let mut ix = [[0i32; 3]; 2];
    let joint = dec.dec_icdf(&tables::STEREO_PRED_JOINT_ICDF, super::tables::ICDF_FTB);
    ix[0][2] = joint / STEREO_PRED_GROUPS;
    ix[1][2] = joint - STEREO_PRED_GROUPS * ix[0][2];

    for side in &mut ix {
        side[0] = dec.dec_icdf(&tables::UNIFORM3_ICDF, super::tables::ICDF_FTB);
        side[1] = dec.dec_icdf(&tables::UNIFORM5_ICDF, super::tables::ICDF_FTB);
    }

    let mut pred_q13 = [0i32; 2];
    for (n, side) in ix.iter_mut().enumerate() {
        side[0] += STEREO_PRED_INTERVALS_PER_GROUP * side[2];
        let low_q13 = tables::STEREO_PRED_QUANT_Q13[side[0] as usize];
        let high_q13 = tables::STEREO_PRED_QUANT_Q13[side[0] as usize + 1];
        let step_q13 = smulww(high_q13 - low_q13, STEREO_STEP_Q16);
        pred_q13[n] = low_q13 + step_q13 * (2 * side[1] + 1);
    }

    pred_q13[0] -= pred_q13[1];
    pred_q13
}

/// Lee con `dec` el indicador de solo medio (`silk_stereo_decode_mid_only`): verdadero si la
/// trama no lleva canal lateral.
pub(super) fn decode_mid_only(dec: &mut EcDec<'_>) -> bool {
    dec.dec_icdf(&tables::STEREO_ONLY_CODE_MID_ICDF, super::tables::ICDF_FTB) != 0
}

/// Convierte en el sitio una trama interna de medio y lateral a izquierdo y derecho: `mid` pasa
/// a ser el izquierdo y `side`, el derecho, retrasados una muestra. El lateral se completa con
/// la predicción `pred_q13`, interpolada desde la de `state` durante los 8 primeros ms a la
/// frecuencia interna `fs_khz`. Actualiza `state` para la trama siguiente.
pub(super) fn ms_to_lr(
    state: &mut StereoState,
    mid: &mut [i16],
    side: &mut [i16],
    pred_q13: [i32; 2],
    fs_khz: u32,
) {
    let frame_length = mid.len();
    debug_assert_eq!(side.len(), frame_length);

    let mut x1 = vec![0i16; frame_length + 2];
    let mut x2 = vec![0i16; frame_length + 2];
    x1[..2].copy_from_slice(&state.s_mid);
    x2[..2].copy_from_slice(&state.s_side);
    x1[2..].copy_from_slice(mid);
    x2[2..].copy_from_slice(side);
    state
        .s_mid
        .copy_from_slice(&x1[frame_length..frame_length + 2]);
    state
        .s_side
        .copy_from_slice(&x2[frame_length..frame_length + 2]);

    let mut pred0_q13 = state.pred_prev_q13[0] as i32;
    let mut pred1_q13 = state.pred_prev_q13[1] as i32;
    let denom_q16 = (1 << 16) / (STEREO_INTERP_LEN_MS as i32 * fs_khz as i32);
    let delta0_q13 = rshift_round(smulbb(pred_q13[0] - pred0_q13, denom_q16), 16);
    let delta1_q13 = rshift_round(smulbb(pred_q13[1] - pred1_q13, denom_q16), 16);
    let interp_len = (STEREO_INTERP_LEN_MS * fs_khz as usize).min(frame_length);

    for n in 0..interp_len {
        pred0_q13 += delta0_q13;
        pred1_q13 += delta1_q13;
        let mut sum = (x1[n] as i32 + x1[n + 2] as i32 + ((x1[n + 1] as i32) << 1)) << 9;
        let sum_after_pred0 = smlawb((x2[n + 1] as i32) << 8, sum, pred0_q13);
        let sum_after_pred1 = smlawb(sum_after_pred0, (x1[n + 1] as i32) << 11, pred1_q13);
        sum = sum_after_pred1;
        x2[n + 1] = sat16(rshift_round(sum, 8));
    }

    pred0_q13 = pred_q13[0];
    pred1_q13 = pred_q13[1];
    for n in interp_len..frame_length {
        let mut sum = (x1[n] as i32 + x1[n + 2] as i32 + ((x1[n + 1] as i32) << 1)) << 9;
        sum = smlawb((x2[n + 1] as i32) << 8, sum, pred0_q13);
        sum = smlawb(sum, (x1[n + 1] as i32) << 11, pred1_q13);
        x2[n + 1] = sat16(rshift_round(sum, 8));
    }
    state.pred_prev_q13 = [pred_q13[0] as i16, pred_q13[1] as i16];

    for n in 0..frame_length {
        let sum = x1[n + 1] as i32 + x2[n + 1] as i32;
        let diff = x1[n + 1] as i32 - x2[n + 1] as i32;
        x1[n + 1] = sat16(sum);
        x2[n + 1] = sat16(diff);
    }
    mid.copy_from_slice(&x1[1..1 + frame_length]);
    side.copy_from_slice(&x2[1..1 + frame_length]);
}

#[cfg(test)]
mod tests {
    use super::StereoState;

    /// Al pasar de mono a estéreo se olvidan los predictores y la historia del lateral, y se
    /// conserva la del medio. La red de equivalencia no lo ejercita: en sus transiciones el
    /// lateral ya está a cero.
    #[test]
    fn reset_mono_to_stereo_keeps_only_mid_history() {
        let mut state = StereoState {
            pred_prev_q13: [11, -12],
            s_mid: [13, -14],
            s_side: [15, -16],
        };
        state.reset_mono_to_stereo();
        assert_eq!(state.pred_prev_q13, [0, 0]);
        assert_eq!(state.s_side, [0, 0]);
        assert_eq!(state.s_mid, [13, -14]);
    }
}
