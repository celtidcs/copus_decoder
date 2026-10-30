/* Flujo de opus_demo con pasos de estéreo a mono y FEC en banda: codifica un PCM de 16 bits
 * estéreo a 48 kHz (por ejemplo, la salida de referencia del vector 02) en tramas de 20 ms, voz,
 * 32 kb/s, banda ancha como mucho, FEC con un 20 % de pérdida declarada, y alterna los canales
 * forzados (estéreo y mono) cada BLOQUE paquetes.
 *   gcc -O2 -I opus-1.6.1/include estereo_fec.c opus-1.6.1/.libs/libopus.a -lm -o estereo_fec
 * Uso: estereo_fec <entrada.pcm> <salida.bit>
 */
#include <stdio.h>
#include "opus.h"

#define TRAMA 960
#define BLOQUE 7
#define MAX_PAQUETE 1500

static void be32(unsigned char *b, opus_uint32 v) {
    b[0] = v >> 24; b[1] = v >> 16; b[2] = v >> 8; b[3] = v;
}

int main(int argc, char **argv) {
    if (argc != 3) return 2;
    FILE *in = fopen(argv[1], "rb"), *out = fopen(argv[2], "wb");
    if (!in || !out) return 1;
    int error, paquetes = 0, con_lbrr = 0, cambios = 0, prev_estereo = -1;
    OpusEncoder *enc = opus_encoder_create(48000, 2, OPUS_APPLICATION_VOIP, &error);
    opus_encoder_ctl(enc, OPUS_SET_BITRATE(32000));
    opus_encoder_ctl(enc, OPUS_SET_MAX_BANDWIDTH(OPUS_BANDWIDTH_WIDEBAND));
    opus_encoder_ctl(enc, OPUS_SET_INBAND_FEC(1));
    opus_encoder_ctl(enc, OPUS_SET_PACKET_LOSS_PERC(20));
    static opus_int16 pcm[TRAMA * 2];
    static unsigned char paquete[MAX_PAQUETE], cabecera[8];
    while (fread(pcm, sizeof(opus_int16), TRAMA * 2, in) == TRAMA * 2) {
        if (paquetes % BLOQUE == 0)
            opus_encoder_ctl(enc, OPUS_SET_FORCE_CHANNELS(paquetes / BLOQUE % 2 ? 1 : 2));
        int n = opus_encode(enc, pcm, TRAMA, paquete, MAX_PAQUETE);
        if (n < 0) return 1;
        opus_uint32 rango;
        opus_encoder_ctl(enc, OPUS_GET_FINAL_RANGE(&rango));
        be32(cabecera, (opus_uint32)n);
        be32(cabecera + 4, rango);
        fwrite(cabecera, 1, 8, out);
        fwrite(paquete, 1, n, out);
        int estereo = opus_packet_get_nb_channels(paquete) == 2;
        if (prev_estereo == 1 && !estereo) cambios++;
        prev_estereo = estereo;
        con_lbrr += opus_packet_has_lbrr(paquete, n) == 1;
        paquetes++;
    }
    printf("%d paquetes, %d con LBRR, %d pasos de estéreo a mono\n", paquetes, con_lbrr, cambios);
    opus_encoder_destroy(enc);
    return 0;
}
