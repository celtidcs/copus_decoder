/* Flujo SILK con DTX codificado con libopus 1.6.1.
 *
 * Uso: casos_silk <estereo.raw> <salida.bit>
 *
 * Entrada: PCM s16le estéreo a 48 kHz, del que se codifica el canal izquierdo (mono), en tramas
 * de 20 ms con la aplicación voip a 16 kb/s, banda ancha como máximo (para que sea SILK y no
 * híbrido) y DTX (OPUS_SET_DTX). De cada PERIODO_SILENCIO tramas, las TRAMAS_SILENCIO últimas se
 * sustituyen por silencio, para que el codificador mande paquetes de DTX (de un byte), que el
 * decodificador trata como pérdidas con ruido de confort. Escribe el flujo en el formato de
 * opus_demo (longitud u32 BE, final_range u32 BE, carga).
 */
#include <stdio.h>
#include <stdlib.h>
#include <opus.h>
#include "salida.h"

#define FS 48000
#define TRAMA 960
#define MAX_PAQUETE 4000
#define TASA 16000
/* Silencio de 0,8 s (40 tramas) cada 3 s (150 tramas): basta para que el codificador entre en
   DTX, que empieza tras unos 200 ms de silencio. */
#define PERIODO_SILENCIO 150
#define TRAMAS_SILENCIO 40

static void u32be(FILE *f, unsigned v) {
    unsigned char b[4] = {(unsigned char)(v >> 24), (unsigned char)(v >> 16),
                          (unsigned char)(v >> 8), (unsigned char)v};
    fwrite(b, 1, 4, f);
}

int main(int argc, char **argv) {
    short estereo[TRAMA * 2], mono[TRAMA];
    unsigned char paquete[MAX_PAQUETE];
    int err, n = 0, i;
    FILE *fe, *fs;
    OpusEncoder *enc;
    if (argc != 3) {
        fprintf(stderr, "uso: casos_silk <estereo.raw> <salida.bit>\n");
        return 1;
    }
    fe = fopen(argv[1], "rb");
    if (!fe) { perror(argv[1]); return 1; }
    fs = crear_salida(argv[2]);
    if (!fs) { perror(argv[2]); return 1; }
    enc = opus_encoder_create(FS, 1, OPUS_APPLICATION_VOIP, &err);
    if (err != OPUS_OK) { fprintf(stderr, "enc %d\n", err); return 1; }
    opus_encoder_ctl(enc, OPUS_SET_SIGNAL(OPUS_SIGNAL_VOICE));
    opus_encoder_ctl(enc, OPUS_SET_BITRATE(TASA));
    opus_encoder_ctl(enc, OPUS_SET_MAX_BANDWIDTH(OPUS_BANDWIDTH_WIDEBAND));
    opus_encoder_ctl(enc, OPUS_SET_DTX(1));
    while (fread(estereo, sizeof estereo, 1, fe) == 1) {
        opus_int32 largo;
        opus_uint32 rango;
        int silencio = n % PERIODO_SILENCIO >= PERIODO_SILENCIO - TRAMAS_SILENCIO;
        for (i = 0; i < TRAMA; i++) mono[i] = silencio ? 0 : estereo[2 * i];
        largo = opus_encode(enc, mono, TRAMA, paquete, MAX_PAQUETE);
        if (largo < 0) { fprintf(stderr, "encode %d\n", (int)largo); return 1; }
        opus_encoder_ctl(enc, OPUS_GET_FINAL_RANGE(&rango));
        u32be(fs, (unsigned)largo);
        u32be(fs, rango);
        fwrite(paquete, 1, (size_t)largo, fs);
        n++;
    }
    fclose(fe);
    if (fclose(fs) != 0) { perror(argv[2]); return 1; }
    opus_encoder_destroy(enc);
    printf("%s: %d paquetes\n", argv[2], n);
    return 0;
}
