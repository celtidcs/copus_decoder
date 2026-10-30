/* Referencia multiflujo de libopus 1.6.1.
 *
 * Uso: multiflujo <caso> <estereo.raw> <mono_fuente.raw> <salida.bit> <salida.dec>
 *
 * Entrada: dos PCM s16le estéreo a 48 kHz. Se forman 3 canales: L y R del primero y el canal
 * izquierdo del segundo. Topología: 2 flujos, 1 acoplado, mapa {0, 1, 2}.
 * Codifica con opus_multistream_encoder según <caso>, escribe el flujo en el formato de
 * opus_demo (longitud u32 BE, final_range u32 BE, carga; longitud 0 = pérdida) y lo decodifica
 * con opus_multistream_decode a s16le entrelazado de 3 canales.
 *
 * Casos:
 *   audio128      aplicación audio, 128 kb/s, tramas de 20 ms.
 *   voip24        aplicación voip, 24 kb/s.
 *   audio64_60ms  aplicación audio, 64 kb/s, tramas de 60 ms.
 *   cbr48         aplicación audio, 48 kb/s, tasa constante.
 *   fec           aplicación voip, 48 kb/s, FEC en banda y 20 % de pérdida declarada.
 *   varios        tramas de 2,5 a 60 ms y tasas de 12 a 192 kb/s, alternadas.
 *
 * Pérdidas (solo en el caso "fec"): paquete n perdido si n % 9 == 8, como el patrón p9 de
 * perdidas.c. Una pérdida seguida de un paquete bueno se decodifica con decode_fec = 1 sobre ese
 * paquete; si no, se oculta. La duración pedida en la pérdida es la del último paquete
 * decodificado, como hace opus_demo.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <opus_multistream.h>

#define FS 48000
#define CANALES 3
#define MAX_TRAMA 5760
#define MAX_PAQUETE 4000

static short *leer(const char *ruta, long *muestras) {
    FILE *f = fopen(ruta, "rb");
    if (!f) { perror(ruta); exit(1); }
    fseek(f, 0, SEEK_END);
    long bytes = ftell(f);
    fseek(f, 0, SEEK_SET);
    short *b = malloc(bytes);
    if (fread(b, 1, bytes, f) != (size_t)bytes) { perror("fread"); exit(1); }
    fclose(f);
    *muestras = bytes / 4; /* estéreo s16 */
    return b;
}

static void u32be(FILE *f, unsigned v) {
    unsigned char b[4] = {v >> 24, v >> 16, v >> 8, v};
    fwrite(b, 1, 4, f);
}

static int perdido(const char *caso, int n) { return strcmp(caso, "fec") == 0 && n % 9 == 8; }

int main(int argc, char **argv) {
    if (argc != 6) {
        fprintf(stderr, "uso: multiflujo <caso> <estereo.raw> <mono_fuente.raw> <salida.bit> <salida.dec>\n");
        return 1;
    }
    const char *caso = argv[1];
    long na, nb;
    short *a = leer(argv[2], &na), *b = leer(argv[3], &nb);
    long n = na < nb ? na : nb;
    const unsigned char mapa[CANALES] = {0, 1, 2};
    int err;
    int app = (!strcmp(caso, "voip24") || !strcmp(caso, "fec")) ? OPUS_APPLICATION_VOIP
                                                                 : OPUS_APPLICATION_AUDIO;
    OpusMSEncoder *enc = opus_multistream_encoder_create(FS, CANALES, 2, 1, mapa, app, &err);
    if (err != OPUS_OK) { fprintf(stderr, "enc %d\n", err); return 1; }
    OpusMSDecoder *dec = opus_multistream_decoder_create(FS, CANALES, 2, 1, mapa, &err);
    if (err != OPUS_OK) { fprintf(stderr, "dec %d\n", err); return 1; }

    int trama = 960;
    if (!strcmp(caso, "audio128")) opus_multistream_encoder_ctl(enc, OPUS_SET_BITRATE(128000));
    else if (!strcmp(caso, "voip24")) opus_multistream_encoder_ctl(enc, OPUS_SET_BITRATE(24000));
    else if (!strcmp(caso, "audio64_60ms")) { opus_multistream_encoder_ctl(enc, OPUS_SET_BITRATE(64000)); trama = 2880; }
    else if (!strcmp(caso, "cbr48")) { opus_multistream_encoder_ctl(enc, OPUS_SET_BITRATE(48000)); opus_multistream_encoder_ctl(enc, OPUS_SET_VBR(0)); }
    else if (!strcmp(caso, "fec")) {
        opus_multistream_encoder_ctl(enc, OPUS_SET_BITRATE(48000));
        opus_multistream_encoder_ctl(enc, OPUS_SET_INBAND_FEC(1));
        opus_multistream_encoder_ctl(enc, OPUS_SET_PACKET_LOSS_PERC(20));
    } else if (strcmp(caso, "varios")) { fprintf(stderr, "caso desconocido\n"); return 1; }
    static const int tramas_varios[] = {120, 240, 480, 960, 1920, 2880, 960, 480};
    static const int tasas_varios[] = {12000, 24000, 48000, 96000, 192000, 32000, 64000};

    /* 1) Codificar todos los paquetes. */
    int cap = 1 << 16, np = 0;
    unsigned char **paq = malloc(cap * sizeof *paq);
    int *len = malloc(cap * sizeof *len);
    unsigned *rng = malloc(cap * sizeof *rng);
    short in[MAX_TRAMA * CANALES];
    for (long pos = 0;; np++) {
        if (!strcmp(caso, "varios")) {
            trama = tramas_varios[np % 8];
            opus_multistream_encoder_ctl(enc, OPUS_SET_BITRATE(tasas_varios[(np / 3) % 7]));
        }
        if (pos + trama > n) break;
        for (int i = 0; i < trama; i++) {
            in[CANALES * i] = a[2 * (pos + i)];
            in[CANALES * i + 1] = a[2 * (pos + i) + 1];
            in[CANALES * i + 2] = b[2 * (pos + i)];
        }
        pos += trama;
        paq[np] = malloc(MAX_PAQUETE);
        len[np] = opus_multistream_encode(enc, in, trama, paq[np], MAX_PAQUETE);
        if (len[np] < 0) { fprintf(stderr, "encode %d\n", len[np]); return 1; }
        opus_multistream_encoder_ctl(enc, OPUS_GET_FINAL_RANGE(&rng[np]));
    }

    /* 2) Escribir el flujo y decodificarlo. */
    FILE *fb = fopen(argv[4], "wb"), *fd = fopen(argv[5], "wb");
    short out[MAX_TRAMA * CANALES];
    int ultima = 960;
    for (int i = 0; i < np; i++) {
        int perd = perdido(caso, i);
        u32be(fb, perd ? 0 : len[i]);
        u32be(fb, perd ? 0 : rng[i]);
        if (!perd) fwrite(paq[i], 1, len[i], fb);
        int r;
        if (perd) {
            int sig = i + 1 < np && !perdido(caso, i + 1);
            r = sig ? opus_multistream_decode(dec, paq[i + 1], len[i + 1], out, ultima, 1)
                    : opus_multistream_decode(dec, NULL, 0, out, ultima, 0);
        } else {
            r = opus_multistream_decode(dec, paq[i], len[i], out, MAX_TRAMA, 0);
        }
        if (r < 0) { fprintf(stderr, "decode %d en %d\n", r, i); return 1; }
        ultima = r;
        fwrite(out, sizeof(short), (size_t)r * CANALES, fd);
    }
    fclose(fb);
    fclose(fd);
    printf("%s: %d paquetes\n", caso, np);
    return 0;
}
