/* Referencia de silk_resampler de libopus 1.6.1 en las bajadas del decodificador (16->12, 16->8
 * y 12->8 kHz) con longitudes de entrada que dejan una muestra suelta al final del bucle por
 * bloques de silk_resampler_private_down_FIR (inLen - Fs_in_kHz = k * bloque + 1).
 * Para cada pareja, una secuencia de llamadas sobre un mismo estado; por llamada imprime la
 * longitud de entrada, las muestras escritas (detectadas con dos centinelas distintos) y la salida.
 * Compilar contra el árbol de libopus:
 *   gcc -O2 -I opus-1.6.1/include -I opus-1.6.1/silk -I opus-1.6.1/celt -I opus-1.6.1 \
 *       resto1.c opus-1.6.1/.libs/libopus.a -lm -o resto1
 * Su salida (./resto1 > remuestreo_resto1.txt) es tests/datos/remuestreo_resto1.txt, que usa la
 * prueba down_fir_matches_libopus_with_one_sample_left de src/silk/resampler.rs. Se generó en
 * Linux x86-64.
 */
#include <stdio.h>
#include <string.h>
#include "SigProc_FIX.h"
#include "resampler_structs.h"

#define MAX_SALIDA 4096
#define AMPLITUD 12000

static unsigned long long estado_aleatorio;

static opus_int16 muestra(void) {
    estado_aleatorio ^= estado_aleatorio << 13;
    estado_aleatorio ^= estado_aleatorio >> 7;
    estado_aleatorio ^= estado_aleatorio << 17;
    return (opus_int16)((long long)(estado_aleatorio % (2 * AMPLITUD + 1)) - AMPLITUD);
}

/* Ejecuta una llamada sobre una copia del estado con el centinela `c` y devuelve la copia. */
static silk_resampler_state_struct llamar(const silk_resampler_state_struct *s, opus_int16 *out,
                                         const opus_int16 *in, int n, opus_int16 c) {
    silk_resampler_state_struct copia = *s;
    for (int i = 0; i < MAX_SALIDA; i++) out[i] = c;
    silk_resampler(&copia, out, in, n);
    return copia;
}

int main(void) {
    const int parejas[3][2] = {{16000, 12000}, {16000, 8000}, {12000, 8000}};
    for (int p = 0; p < 3; p++) {
        int fs_in = parejas[p][0], fs_out = parejas[p][1], k = fs_in / 1000, bloque = 10 * k;
        int longitudes[6] = {k + bloque + 1, 2 * bloque, k + 1, k + 2 * bloque + 1, 3 * k + 7, bloque};
        silk_resampler_state_struct s;
        silk_resampler_init(&s, fs_in, fs_out, 0);
        estado_aleatorio = 0x9E3779B97F4A7C15ULL + (unsigned long long)p;
        printf("pareja %d %d\n", fs_in, fs_out);
        for (int l = 0; l < 6; l++) {
            static opus_int16 in[MAX_SALIDA], a[MAX_SALIDA], b[MAX_SALIDA];
            int n = longitudes[l];
            for (int i = 0; i < n; i++) in[i] = muestra();
            silk_resampler_state_struct sa = llamar(&s, a, in, n, 0x7ABC);
            llamar(&s, b, in, n, -0x1234);
            int escritas = 0;
            for (int i = 0; i < MAX_SALIDA; i++)
                if (a[i] == b[i]) escritas = i + 1;
            s = sa;
            printf("llamada %d entrada", n);
            for (int i = 0; i < n; i++) printf(" %d", in[i]);
            printf("\nsalida %d", escritas);
            for (int i = 0; i < escritas; i++) printf(" %d", a[i]);
            printf("\n");
        }
    }
    return 0;
}
