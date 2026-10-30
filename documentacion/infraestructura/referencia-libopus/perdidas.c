/* Pierde paquetes de un flujo .bit de opus_demo según un patrón fijo.
 *
 * Uso: perdidas <entrada.bit> <p9|r3> <salida.bit>
 *
 * Formato de opus_demo (src/opus_demo.c de libopus 1.6.1): cada registro lleva la longitud de la
 * carga (u32 big endian), el final_range del codificador (u32 big endian) y la carga. Un registro
 * de longitud 0 es un paquete perdido, tanto para opus_demo como para las pruebas del proyecto.
 *
 * Patrones (n es el índice del paquete, desde 0):
 *   p9: se pierde el paquete n si n % 9 == 8 (pérdidas sueltas, una de cada nueve).
 *   r3: se pierde el paquete n si n % 50 >= 47 (ráfagas de tres pérdidas seguidas).
 * El registro perdido se escribe con longitud 0 y final_range 0, sin carga.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "salida.h"

/* Carga máxima que se acepta en un registro: un paquete Opus no pasa de 1275 bytes por trama y
   48 tramas, así que 64 KiB sobra; un valor mayor indica un archivo dañado. */
#define MAX_CARGA 65536

static int leer_u32(FILE *f, unsigned long *v) {
    unsigned char b[4];
    if (fread(b, 1, 4, f) != 4) return 0;
    *v = ((unsigned long)b[0] << 24) | ((unsigned long)b[1] << 16) | ((unsigned long)b[2] << 8) | b[3];
    return 1;
}

static void escribir_u32(FILE *f, unsigned long v) {
    unsigned char b[4] = {(unsigned char)(v >> 24), (unsigned char)(v >> 16),
                          (unsigned char)(v >> 8), (unsigned char)v};
    fwrite(b, 1, 4, f);
}

static int perdido(const char *patron, long n) {
    return strcmp(patron, "p9") == 0 ? n % 9 == 8 : n % 50 >= 47;
}

int main(int argc, char **argv) {
    static unsigned char carga[MAX_CARGA];
    unsigned long largo, rango;
    long n = 0, perdidos = 0;
    FILE *ent, *sal;
    if (argc != 4 || (strcmp(argv[2], "p9") != 0 && strcmp(argv[2], "r3") != 0)) {
        fprintf(stderr, "uso: perdidas <entrada.bit> <p9|r3> <salida.bit>\n");
        return 1;
    }
    ent = fopen(argv[1], "rb");
    if (!ent) { perror(argv[1]); return 1; }
    sal = crear_salida(argv[3]);
    if (!sal) { perror(argv[3]); return 1; }
    while (leer_u32(ent, &largo)) {
        if (!leer_u32(ent, &rango) || largo > MAX_CARGA || fread(carga, 1, largo, ent) != largo) {
            fprintf(stderr, "%s: registro %ld truncado o dañado\n", argv[1], n);
            return 1;
        }
        if (perdido(argv[2], n)) {
            escribir_u32(sal, 0);
            escribir_u32(sal, 0);
            perdidos++;
        } else {
            escribir_u32(sal, largo);
            escribir_u32(sal, rango);
            fwrite(carga, 1, largo, sal);
        }
        n++;
    }
    fclose(ent);
    if (fclose(sal) != 0) { perror(argv[3]); return 1; }
    printf("%s: %ld paquetes, %ld perdidos\n", argv[3], n, perdidos);
    return 0;
}
