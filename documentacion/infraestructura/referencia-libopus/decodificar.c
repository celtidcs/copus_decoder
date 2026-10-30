/* Decodifica un flujo .bit de opus_demo con la API opus_decode de libopus (PCM i16) y escribe la
 * salida en s16le, entrelazada si hay dos canales.
 *
 * Uso: decodificar <frecuencia> <canales> <entrada.bit> <salida.dec>
 *
 * Sigue la política de pérdidas de opus_demo -d (src/opus_demo.c de libopus 1.6.1, líneas
 * 1118-1212): un registro de longitud 0 es un paquete perdido; las pérdidas se decodifican al
 * llegar el siguiente paquete bueno, cada una con la duración del último paquete
 * (OPUS_GET_LAST_PACKET_DURATION), la última de la racha con FEC si ese paquete trae LBRR
 * (opus_packet_has_lbrr) y las demás ocultadas; las pérdidas del final del flujo no se emiten.
 *
 * A diferencia de opus_demo, que decodifica con opus_decode24 y pasa a 16 bits con (s + 128) >> 8
 * (dos redondeos), aquí la salida es la de opus_decode: un solo redondeo, al par
 * (celt_float2int16), que es el contrato de OpusDecoder::decode de este proyecto.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "opus.h"

/* Carga máxima de un registro: un paquete Opus no pasa de 1275 bytes por trama y 48 tramas, así
   que 64 KiB sobra; un valor mayor indica un archivo dañado. */
#define MAX_CARGA 65536
/* Duración máxima de un paquete Opus: 120 ms. */
#define MAX_TRAMA_MS 120
/* Frecuencia más alta de salida de libopus. */
#define MAX_FRECUENCIA 48000
#define MAX_CANALES 2
#define MAX_MUESTRAS (MAX_FRECUENCIA / 1000 * MAX_TRAMA_MS)

static int leer_u32(FILE *f, unsigned long *v) {
    unsigned char b[4];
    if (fread(b, 1, 4, f) != 4) return 0;
    *v = ((unsigned long)b[0] << 24) | ((unsigned long)b[1] << 16) | ((unsigned long)b[2] << 8) | b[3];
    return 1;
}

/* Escribe n muestras por canal de pcm en s16le. Devuelve 0 si falla la escritura. */
static int escribir_s16le(FILE *f, const opus_int16 *pcm, int n, int canales) {
    static unsigned char bytes[MAX_MUESTRAS * MAX_CANALES * 2];
    int i;
    for (i = 0; i < n * canales; i++) {
        bytes[2 * i] = (unsigned char)(pcm[i] & 0xFF);
        bytes[2 * i + 1] = (unsigned char)((pcm[i] >> 8) & 0xFF);
    }
    return fwrite(bytes, 2, (size_t)n * canales, f) == (size_t)n * canales;
}

/* Decodifica una trama y la escribe; un error de libopus se avisa y la trama no se emite, como en
   opus_demo. Devuelve 0 solo si falla la escritura. */
static int decodificar_trama(OpusDecoder *dec, const unsigned char *datos, opus_int32 len,
                             int muestras, int fec, int canales, FILE *salida, long registro) {
    static opus_int16 pcm[MAX_MUESTRAS * MAX_CANALES];
    int n = opus_decode(dec, datos, len, pcm, muestras, fec);
    if (n < 0) {
        fprintf(stderr, "registro %ld: error al decodificar: %s\n", registro, opus_strerror(n));
        return 1;
    }
    return escribir_s16le(salida, pcm, n, canales);
}

int main(int argc, char **argv) {
    static unsigned char carga[MAX_CARGA];
    if (argc != 5) {
        fprintf(stderr, "uso: %s <frecuencia> <canales> <entrada.bit> <salida.dec>\n", argv[0]);
        return 2;
    }
    opus_int32 frecuencia = atoi(argv[1]);
    int canales = atoi(argv[2]);
    FILE *entrada = fopen(argv[3], "rb");
    FILE *salida = fopen(argv[4], "wb");
    if (!entrada || !salida) {
        fprintf(stderr, "no se puede abrir %s o %s\n", argv[3], argv[4]);
        return 1;
    }
    int err;
    OpusDecoder *dec = opus_decoder_create(frecuencia, canales, &err);
    if (err != OPUS_OK) {
        fprintf(stderr, "opus_decoder_create: %s\n", opus_strerror(err));
        return 1;
    }
    unsigned long len, rango;
    long registro = 0;
    int perdidas = 0;
    while (leer_u32(entrada, &len) && leer_u32(entrada, &rango)) {
        if (len > MAX_CARGA || fread(carga, 1, len, entrada) != len) {
            fprintf(stderr, "registro %ld: carga de %lu bytes dañada\n", registro, len);
            return 1;
        }
        if (len == 0) {
            perdidas++;
            registro++;
            continue;
        }
        int lbrr = opus_packet_has_lbrr(carga, (opus_int32)len) > 0;
        for (int fr = 0; fr < perdidas; fr++) {
            opus_int32 duracion = 0;
            opus_decoder_ctl(dec, OPUS_GET_LAST_PACKET_DURATION(&duracion));
            int con_fec = fr == perdidas - 1 && lbrr;
            if (!decodificar_trama(dec, con_fec ? carga : NULL, con_fec ? (opus_int32)len : 0,
                                   duracion, con_fec, canales, salida, registro))
                return 1;
        }
        perdidas = 0;
        if (!decodificar_trama(dec, carga, (opus_int32)len, MAX_MUESTRAS, 0, canales, salida,
                               registro))
            return 1;
        registro++;
    }
    opus_decoder_destroy(dec);
    fclose(entrada);
    return fclose(salida) == 0 ? 0 : 1;
}
