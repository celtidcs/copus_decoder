//! Decodificación entrópica de los índices de SILK (`silk_decode_indices`) y de sus pulsos
//! (`silk_decode_pulses`), común a las tramas LBRR y a la carga principal.

use crate::Error;
use crate::entropy::EcDec;

use super::entropy_tables;
use super::ltp::LtpPeriodicity;
use super::tables;
use super::tables::{ICDF_FTB, MAX_LPC_ORDER, MAX_NB_SUBFR, NLSF_INTERP_NONE_Q2, TYPE_VOICED};

const MAX_NB_SHELL_BLOCKS: usize = 20;
/// Con VAD, el índice conjunto de tipo y desplazamiento empieza en 2: los dos primeros (sin
/// actividad de voz) solo se codifican sin VAD (`silk_decode_indices`).
const TYPE_OFFSET_VAD_FIRST: i32 = 2;
/// Bits bajos de la ganancia absoluta de la primera subtrama, codificados con `UNIFORM8_ICDF`
/// (`silk_decode_indices`).
const GAIN_LSB_BITS: u32 = 3;
/// Desplazamiento del delta de lag: el símbolo 0 pide lag absoluto y los demás valen
/// `símbolo - 9`, de -8 a +11 (`silk_decode_indices`).
const PITCH_DELTA_OFFSET: i32 = 9;
/// Desplazamientos LSB a partir de los cuales ya no se admite otro escape: la ICDF se lee desde
/// su segunda posición (`silk_decode_pulses`).
const MAX_PULSE_LSHIFTS: i32 = 10;
/// Posición del número de desplazamientos LSB dentro de `sum_pulses`; los 5 bits bajos
/// conservan el recuento de pulsos (`silk_decode_pulses` y `silk_decode_signs`).
const SUM_PULSES_LSHIFT_POS: i32 = 5;
const SUM_PULSES_COUNT_MASK: i32 = (1 << SUM_PULSES_LSHIFT_POS) - 1;
/// Longitud de cada fila de `SIGN_ICDF`: una entrada por recuento de pulsos de 0 a 6, y 6 vale
/// para cualquier recuento mayor (`silk_decode_signs`).
const SIGN_ICDF_ROW_LEN: usize = 7;
const SIGN_ICDF_MAX_INDEX: i32 = SIGN_ICDF_ROW_LEN as i32 - 1;
const NLSF_STAGE2_ROW: usize = (2 * entropy_tables::NLSF_QUANT_MAX_AMPLITUDE + 1) as usize;

/// Modo de codificación de los índices de una trama interna de SILK respecto a la anterior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CondCoding {
    /// Codificación independiente (`CODE_INDEPENDENTLY`).
    Independently,
    /// Codificación independiente sin escala LTP (`CODE_INDEPENDENTLY_NO_LTP_SCALING`).
    IndependentlyNoLtpScaling,
    /// Codificación condicional (`CODE_CONDITIONALLY`).
    Conditionally,
}

/// Estado de un canal que necesita la decodificación de los índices de SILK.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct ChannelState {
    /// Frecuencia interna de SILK, en kHz.
    pub fs_khz: u32,
    /// Subtramas de la trama interna actual (2 en las de 10 ms y 4 en las de 20 ms).
    pub nb_subfr: usize,
    /// Tipo de señal de la trama anterior, que condiciona la decodificación.
    pub ec_prev_signal_type: i32,
    /// Índice de lag de la trama anterior, base del lag relativo.
    pub ec_prev_lag_index: i32,
}

/// Información lateral decodificada de una trama interna.
#[derive(Debug, Clone, Copy)]
pub(super) struct DecodedSideInfo {
    /// Tipo de señal (sin voz, sorda o sonora).
    pub signal_type: i32,
    /// Tipo de desplazamiento del cuantificador.
    pub quant_offset_type: i32,
    /// Índices NLSF de la trama.
    pub nlsf: DecodedNlsfIndices,
    /// Índices de ganancia de todas las subtramas.
    pub gain_indices: [i8; MAX_NB_SUBFR],
    /// Índice absoluto del lag de tono, en tramas sonoras.
    pub lag_index: i16,
    /// Índice del contorno de tono, en tramas sonoras.
    pub contour_index: i8,
    /// Clase de periodicidad del LTP.
    pub ltp_periodicity: LtpPeriodicity,
    /// Índice del libro de códigos LTP de cada subtrama.
    pub ltp_indices: [i8; MAX_NB_SUBFR],
    /// Índice de la escala LTP.
    pub ltp_scale_index: i8,
    /// Semilla del generador pseudoaleatorio de la excitación.
    pub seed: i8,
}

/// Índices NLSF de SILK: el de la primera etapa y los residuos de la segunda.
#[derive(Debug, Clone, Copy)]
pub(super) struct DecodedNlsfIndices {
    /// Índices agrupados como `[I1, res0, ..., res(order-1)]`.
    pub values: [i8; MAX_LPC_ORDER + 1],
    /// Factor de interpolación, en Q2.
    pub interp_coef_q2: i8,
}

/// Fija en `state` la frecuencia interna `fs_khz` y las `nb_subfr` subtramas de la trama actual.
pub(super) fn configure_channel_state(state: &mut ChannelState, fs_khz: u32, nb_subfr: usize) {
    state.fs_khz = fs_khz;
    state.nb_subfr = nb_subfr;
}

/// Índices del tono y del filtro LTP de una trama sonora; en las demás tramas valen cero.
#[derive(Debug, Clone, Copy, Default)]
struct PitchIndices {
    lag_index: i16,
    contour_index: i8,
    ltp_periodicity: LtpPeriodicity,
    ltp_indices: [i8; MAX_NB_SUBFR],
    ltp_scale_index: i8,
}

/// Decodifica los índices de una trama interna de SILK (`silk_decode_indices`), en el orden del
/// flujo: tipo de señal y desplazamiento, ganancias, NLSF, interpolación, tono (en tramas
/// sonoras) y semilla.
///
/// `frame_vad` es el bit de actividad de voz de la trama y `decode_lbrr`, si es una trama LBRR
/// (que siempre se codifica como activa). Actualiza en `state` el contexto de la trama
/// siguiente.
///
/// # Errores
///
/// `BadPacket` si un índice queda fuera de su libro de códigos.
pub(super) fn decode_indices(
    state: &mut ChannelState,
    dec: &mut EcDec<'_>,
    frame_vad: bool,
    decode_lbrr: bool,
    cond_coding: CondCoding,
) -> Result<DecodedSideInfo, Error> {
    let ix = if decode_lbrr || frame_vad {
        dec.dec_icdf(&entropy_tables::TYPE_OFFSET_VAD_ICDF, ICDF_FTB) + TYPE_OFFSET_VAD_FIRST
    } else {
        dec.dec_icdf(&entropy_tables::TYPE_OFFSET_NO_VAD_ICDF, ICDF_FTB)
    };
    let signal_type = ix >> 1;
    let quant_offset_type = ix & 1;
    let gain_indices = decode_gain_indices(dec, state.nb_subfr, signal_type, cond_coding);
    let nlsf = decode_nlsf_indices(dec, state, signal_type)?;
    let pitch = if signal_type == TYPE_VOICED {
        decode_pitch_indices(state, dec, cond_coding)?
    } else {
        PitchIndices::default()
    };
    state.ec_prev_signal_type = signal_type;
    let seed = dec.dec_icdf(&entropy_tables::UNIFORM4_ICDF, ICDF_FTB) as i8;

    Ok(DecodedSideInfo {
        signal_type,
        quant_offset_type,
        nlsf,
        gain_indices,
        lag_index: pitch.lag_index,
        contour_index: pitch.contour_index,
        ltp_periodicity: pitch.ltp_periodicity,
        ltp_indices: pitch.ltp_indices,
        ltp_scale_index: pitch.ltp_scale_index,
        seed,
    })
}

/// Índices de ganancia de las `nb_subfr` subtramas: la primera, absoluta o (con codificación
/// condicional) relativa a la trama anterior; las demás, relativas a la subtrama previa.
fn decode_gain_indices(
    dec: &mut EcDec<'_>,
    nb_subfr: usize,
    signal_type: i32,
    cond_coding: CondCoding,
) -> [i8; MAX_NB_SUBFR] {
    let mut gain_indices = [0i8; MAX_NB_SUBFR];
    if cond_coding == CondCoding::Conditionally {
        gain_indices[0] = dec.dec_icdf(&entropy_tables::DELTA_GAIN_ICDF, ICDF_FTB) as i8;
    } else {
        gain_indices[0] = (dec.dec_icdf(&entropy_tables::GAIN_ICDF[signal_type as usize], ICDF_FTB)
            << GAIN_LSB_BITS) as i8;
        gain_indices[0] += dec.dec_icdf(&entropy_tables::UNIFORM8_ICDF, ICDF_FTB) as i8;
    }
    for gain in gain_indices.iter_mut().take(nb_subfr).skip(1) {
        *gain = dec.dec_icdf(&entropy_tables::DELTA_GAIN_ICDF, ICDF_FTB) as i8;
    }
    gain_indices
}

/// Índices NLSF de la trama: el vector de la primera etapa, los residuos de la segunda (con
/// extensión en los extremos) y, en tramas de 20 ms, el factor de interpolación.
///
/// # Errores
///
/// `BadPacket` si el vector de la primera etapa no existe en el libro de códigos.
fn decode_nlsf_indices(
    dec: &mut EcDec<'_>,
    state: &ChannelState,
    signal_type: i32,
) -> Result<DecodedNlsfIndices, Error> {
    let cb = tables::nlsf_codebook(state.fs_khz);
    let cb1_offset = ((signal_type >> 1) as usize) * cb.n_vectors;
    let cb1_index = dec.dec_icdf(&cb.cb1_icdf[cb1_offset..], ICDF_FTB) as usize;
    if cb1_index >= cb.n_vectors {
        return Err(Error::BadPacket);
    }
    let mut values = [0i8; MAX_LPC_ORDER + 1];
    values[0] = cb1_index as i8;

    let mut ec_ix = [0usize; MAX_LPC_ORDER];
    unpack_nlsf_ec_ix(&mut ec_ix, cb, cb1_index)?;
    for (value, &row) in values[1..].iter_mut().zip(ec_ix.iter().take(cb.order)) {
        let mut stage2 = dec.dec_icdf(&cb.ec_icdf[row..], ICDF_FTB);
        if stage2 == 0 {
            stage2 -= dec.dec_icdf(&entropy_tables::NLSF_EXT_ICDF, ICDF_FTB);
        } else if stage2 == 2 * entropy_tables::NLSF_QUANT_MAX_AMPLITUDE {
            stage2 += dec.dec_icdf(&entropy_tables::NLSF_EXT_ICDF, ICDF_FTB);
        }
        *value = (stage2 - entropy_tables::NLSF_QUANT_MAX_AMPLITUDE) as i8;
    }

    let interp_coef_q2 = if state.nb_subfr == MAX_NB_SUBFR {
        dec.dec_icdf(&entropy_tables::NLSF_INTERP_FACTOR_ICDF, ICDF_FTB) as i8
    } else {
        NLSF_INTERP_NONE_Q2
    };
    Ok(DecodedNlsfIndices {
        values,
        interp_coef_q2,
    })
}

/// Índices de tono de una trama sonora: lag, contorno, periodicidad y libro LTP de cada
/// subtrama y, en tramas independientes, la escala LTP.
///
/// # Errores
///
/// `BadPacket` si la periodicidad LTP no existe.
fn decode_pitch_indices(
    state: &mut ChannelState,
    dec: &mut EcDec<'_>,
    cond_coding: CondCoding,
) -> Result<PitchIndices, Error> {
    let lag_index = decode_lag_index(state, dec, cond_coding) as i16;
    let contour_index = dec.dec_icdf(
        entropy_tables::pitch_contour_icdf(state.fs_khz, state.nb_subfr),
        ICDF_FTB,
    ) as i8;
    let ltp_periodicity =
        LtpPeriodicity::from_index(dec.dec_icdf(&entropy_tables::LTP_PER_INDEX_ICDF, ICDF_FTB))
            .ok_or(Error::BadPacket)?;
    let ltp_gain_icdf = ltp_periodicity.gain_icdf();
    let mut ltp_indices = [0i8; MAX_NB_SUBFR];
    for ltp_index in ltp_indices.iter_mut().take(state.nb_subfr) {
        *ltp_index = dec.dec_icdf(ltp_gain_icdf, ICDF_FTB) as i8;
    }
    let ltp_scale_index = if cond_coding == CondCoding::Independently {
        dec.dec_icdf(&entropy_tables::LTPSCALE_ICDF, ICDF_FTB) as i8
    } else {
        0
    };
    Ok(PitchIndices {
        lag_index,
        contour_index,
        ltp_periodicity,
        ltp_indices,
        ltp_scale_index,
    })
}

/// Índice de lag de una trama sonora: relativo al de la trama anterior si esta también era
/// sonora y la codificación es condicional (salvo que el símbolo pida lag absoluto), o absoluto,
/// en una parte alta y otra baja. Lo guarda en `state` para la trama siguiente.
fn decode_lag_index(state: &mut ChannelState, dec: &mut EcDec<'_>, cond_coding: CondCoding) -> i32 {
    if cond_coding == CondCoding::Conditionally && state.ec_prev_signal_type == TYPE_VOICED {
        let delta_lag = dec.dec_icdf(&entropy_tables::PITCH_DELTA_ICDF, ICDF_FTB);
        if delta_lag > 0 {
            state.ec_prev_lag_index += delta_lag - PITCH_DELTA_OFFSET;
            return state.ec_prev_lag_index;
        }
    }
    state.ec_prev_lag_index = dec.dec_icdf(&entropy_tables::PITCH_LAG_ICDF, ICDF_FTB)
        * ((state.fs_khz as i32) >> 1)
        + dec.dec_icdf(
            entropy_tables::pitch_lag_low_bits_icdf(state.fs_khz),
            ICDF_FTB,
        );
    state.ec_prev_lag_index
}

/// Decodifica los pulsos de una trama interna de `frame_length` muestras (`silk_decode_pulses`):
/// nivel de tasa, suma de pulsos de cada bloque, su reparto, los bits bajos y los signos, que
/// dependen de `signal_type` y `quant_offset_type`. Devuelve los pulsos con signo, recortados a
/// `frame_length`.
///
/// # Errores
///
/// `BadPacket` si la trama necesita más de `MAX_NB_SHELL_BLOCKS` bloques o si un símbolo queda
/// fuera de sus tablas.
pub(super) fn decode_pulses(
    dec: &mut EcDec<'_>,
    signal_type: i32,
    quant_offset_type: i32,
    frame_length: usize,
) -> Result<Vec<i16>, Error> {
    let rate_level = dec.dec_icdf(
        &entropy_tables::RATE_LEVELS_ICDF[(signal_type >> 1) as usize],
        ICDF_FTB,
    ) as usize;
    if rate_level >= entropy_tables::N_RATE_LEVELS {
        return Err(Error::BadPacket);
    }

    let mut iter = frame_length / entropy_tables::SHELL_CODEC_FRAME_LENGTH;
    if iter * entropy_tables::SHELL_CODEC_FRAME_LENGTH < frame_length {
        iter += 1;
    }
    if iter > MAX_NB_SHELL_BLOCKS {
        return Err(Error::BadPacket);
    }

    let mut sum_pulses = [0i32; MAX_NB_SHELL_BLOCKS];
    let mut n_lshifts = [0i32; MAX_NB_SHELL_BLOCKS];
    for (sum, lshifts) in sum_pulses.iter_mut().zip(&mut n_lshifts).take(iter) {
        (*sum, *lshifts) = decode_block_sum(dec, rate_level);
    }

    let mut pulses = vec![0i16; iter * entropy_tables::SHELL_CODEC_FRAME_LENGTH];
    for (block, &sum) in pulses
        .chunks_exact_mut(entropy_tables::SHELL_CODEC_FRAME_LENGTH)
        .zip(&sum_pulses)
    {
        if sum > 0 {
            shell_decoder(block, dec, sum)?;
        } else {
            block.fill(0);
        }
    }

    for ((block, sum), &lshifts) in pulses
        .chunks_exact_mut(entropy_tables::SHELL_CODEC_FRAME_LENGTH)
        .zip(&mut sum_pulses)
        .zip(&n_lshifts)
    {
        if lshifts > 0 {
            decode_lsbs(dec, block, lshifts);
            *sum |= lshifts << SUM_PULSES_LSHIFT_POS;
        }
    }

    decode_signs(
        dec,
        &mut pulses,
        signal_type,
        quant_offset_type,
        &sum_pulses,
        iter,
    )?;
    pulses.truncate(frame_length);
    Ok(pulses)
}

/// Suma de pulsos de un bloque de la capa de conchas y cuántos desplazamientos LSB la escapan:
/// mientras el símbolo pide más de `SILK_MAX_PULSES`, se lee otro en la fila del nivel más alto,
/// desde su segunda posición tras `MAX_PULSE_LSHIFTS` escapes.
fn decode_block_sum(dec: &mut EcDec<'_>, rate_level: usize) -> (i32, i32) {
    let mut sum = dec.dec_icdf(&entropy_tables::PULSES_PER_BLOCK_ICDF[rate_level], ICDF_FTB);
    let mut lshifts = 0i32;
    while sum == entropy_tables::SILK_MAX_PULSES + 1 {
        lshifts += 1;
        let row = entropy_tables::N_RATE_LEVELS - 1;
        let offset = usize::from(lshifts == MAX_PULSE_LSHIFTS);
        sum = dec.dec_icdf(
            &entropy_tables::PULSES_PER_BLOCK_ICDF[row][offset..],
            ICDF_FTB,
        );
    }
    (sum, lshifts)
}

/// Añade a cada magnitud del bloque sus `lshifts` bits bajos, del más alto al más bajo.
fn decode_lsbs(dec: &mut EcDec<'_>, block: &mut [i16], lshifts: i32) {
    for sample in block.iter_mut() {
        let mut abs_q = *sample as i32;
        for _ in 0..lshifts {
            abs_q = (abs_q << 1) + dec.dec_icdf(&entropy_tables::LSB_ICDF, ICDF_FTB);
        }
        *sample = abs_q as i16;
    }
}

/// Escribe en `out_rows` el desplazamiento de la fila de la ICDF de etapa 2 de cada coeficiente,
/// según los selectores del vector `cb1_index` del libro `cb`. Falla con `BadPacket` si los
/// selectores de ese vector se salen de la tabla.
fn unpack_nlsf_ec_ix(
    out_rows: &mut [usize; MAX_LPC_ORDER],
    cb: &tables::NlsfCodebook,
    cb1_index: usize,
) -> Result<(), Error> {
    let sel_base = cb1_index * cb.order / 2;
    if sel_base + cb.order / 2 > cb.ec_sel.len() {
        return Err(Error::BadPacket);
    }
    for i in (0..cb.order).step_by(2) {
        let entry = cb.ec_sel[sel_base + i / 2];
        for (j, &shift) in entropy_tables::NLSF_EC_SEL_ROW_SHIFTS.iter().enumerate() {
            let row = (entry >> shift) & entropy_tables::NLSF_EC_SEL_ROW_MASK;
            out_rows[i + j] = row as usize * NLSF_STAGE2_ROW;
        }
    }
    Ok(())
}

/// Reparte los `p` pulsos de un nodo de la capa de conchas entre sus dos hijos con la tabla
/// `shell_table` y devuelve el recuento de cada uno. Falla con `BadPacket` si `p` o su
/// desplazamiento quedan fuera de las tablas.
fn decode_split(dec: &mut EcDec<'_>, p: i32, shell_table: &[u8]) -> Result<(i16, i16), Error> {
    if p <= 0 {
        return Ok((0, 0));
    }
    let p_usize = p as usize;
    if p_usize >= entropy_tables::SHELL_CODE_TABLE_OFFSETS.len() {
        return Err(Error::BadPacket);
    }
    let offset = entropy_tables::SHELL_CODE_TABLE_OFFSETS[p_usize] as usize;
    if offset >= shell_table.len() {
        return Err(Error::BadPacket);
    }
    let child1 = dec.dec_icdf(&shell_table[offset..], ICDF_FTB) as i16;
    let child2 = (p as i16).saturating_sub(child1);
    Ok((child1, child2))
}

/// Reparte los `pulses4` pulsos de un bloque de 16 muestras de la capa de conchas entre sus
/// muestras (`silk_shell_decoder`) y escribe las magnitudes en `out_block`. Falla con
/// `BadPacket` si el bloque no tiene 16 muestras o si un reparto falla.
fn shell_decoder(out_block: &mut [i16], dec: &mut EcDec<'_>, pulses4: i32) -> Result<(), Error> {
    if out_block.len() != entropy_tables::SHELL_CODEC_FRAME_LENGTH {
        return Err(Error::BadPacket);
    }
    let (p30, p31) = decode_split(dec, pulses4, &entropy_tables::SHELL_CODE_TABLE3)?;
    let (p20, p21) = decode_split(dec, p30 as i32, &entropy_tables::SHELL_CODE_TABLE2)?;
    let (p10, p11) = decode_split(dec, p20 as i32, &entropy_tables::SHELL_CODE_TABLE1)?;
    let (p00, p01) = decode_split(dec, p10 as i32, &entropy_tables::SHELL_CODE_TABLE0)?;
    let (p02, p03) = decode_split(dec, p11 as i32, &entropy_tables::SHELL_CODE_TABLE0)?;
    let (p12, p13) = decode_split(dec, p21 as i32, &entropy_tables::SHELL_CODE_TABLE1)?;
    let (p04, p05) = decode_split(dec, p12 as i32, &entropy_tables::SHELL_CODE_TABLE0)?;
    let (p06, p07) = decode_split(dec, p13 as i32, &entropy_tables::SHELL_CODE_TABLE0)?;
    let (p22, p23) = decode_split(dec, p31 as i32, &entropy_tables::SHELL_CODE_TABLE2)?;
    let (p14, p15) = decode_split(dec, p22 as i32, &entropy_tables::SHELL_CODE_TABLE1)?;
    let (p08, p09) = decode_split(dec, p14 as i32, &entropy_tables::SHELL_CODE_TABLE0)?;
    let (p10b, p11b) = decode_split(dec, p15 as i32, &entropy_tables::SHELL_CODE_TABLE0)?;
    let (p16, p17) = decode_split(dec, p23 as i32, &entropy_tables::SHELL_CODE_TABLE1)?;
    let (p12b, p13b) = decode_split(dec, p16 as i32, &entropy_tables::SHELL_CODE_TABLE0)?;
    let (p14b, p15b) = decode_split(dec, p17 as i32, &entropy_tables::SHELL_CODE_TABLE0)?;
    out_block.copy_from_slice(&[
        p00, p01, p02, p03, p04, p05, p06, p07, p08, p09, p10b, p11b, p12b, p13b, p14b, p15b,
    ]);
    Ok(())
}

/// Lee el signo de cada magnitud no nula de los `iter` primeros bloques de `pulses`
/// (`silk_decode_signs`), con la ICDF que eligen `signal_type`, `quant_offset_type` y el
/// recuento de pulsos del bloque en `sum_pulses`. Falla con `BadPacket` si `pulses` es más corto
/// que los bloques o si la fila de signos queda fuera de la tabla.
fn decode_signs(
    dec: &mut EcDec<'_>,
    pulses: &mut [i16],
    signal_type: i32,
    quant_offset_type: i32,
    sum_pulses: &[i32; MAX_NB_SHELL_BLOCKS],
    iter: usize,
) -> Result<(), Error> {
    if pulses.len() < iter * entropy_tables::SHELL_CODEC_FRAME_LENGTH {
        return Err(Error::BadPacket);
    }
    let base = SIGN_ICDF_ROW_LEN * (quant_offset_type + (signal_type << 1)) as usize;
    if base + SIGN_ICDF_MAX_INDEX as usize >= entropy_tables::SIGN_ICDF.len() {
        return Err(Error::BadPacket);
    }
    for (i, &p) in sum_pulses.iter().take(iter).enumerate() {
        if p <= 0 {
            continue;
        }
        let idx = (p & SUM_PULSES_COUNT_MASK).min(SIGN_ICDF_MAX_INDEX) as usize;
        let mut icdf = [0u8; 2];
        icdf[0] = entropy_tables::SIGN_ICDF[base + idx];
        let block = &mut pulses[i * entropy_tables::SHELL_CODEC_FRAME_LENGTH
            ..(i + 1) * entropy_tables::SHELL_CODEC_FRAME_LENGTH];
        for sample in block.iter_mut() {
            if *sample > 0 {
                let sign = dec.dec_icdf(&icdf, ICDF_FTB);
                if sign == 0 {
                    *sample = -*sample;
                }
            }
        }
    }
    Ok(())
}
