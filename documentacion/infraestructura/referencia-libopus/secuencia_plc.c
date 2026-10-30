/* Construye un flujo .bit de opus_demo con una racha de pérdidas entre dos bloques de paquetes.
 *
 * Uso: secuencia_plc <entrada.bit> <buenos> <perdidas> <salida.bit>
 *
 * La salida lleva los <buenos> primeros paquetes de la entrada, <perdidas> registros vacíos (que
 * opus_demo -d decodifica como pérdidas) y los <buenos> paquetes siguientes. Los registros vacíos
 * de la entrada se saltan. Cada paquete conserva su longitud, su final_range y su carga, así que
 * opus_demo -d sigue comprobando el estado del codificador de rango; los registros vacíos llevan
 * el estado a 0.
 */
#include <stdio.h>
#include <stdlib.h>
#include "salida.h"

/* Carga máxima que se acepta en un registro (ver perdidas.c). */
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

/* Copia a sal los n siguientes paquetes no vacíos de ent. Devuelve 0 si los hay y -1 si no. */
static int copiar_paquetes(FILE *ent, FILE *sal, int n) {
    static unsigned char carga[MAX_CARGA];
    unsigned long largo, rango;
    while (n > 0) {
        if (!leer_u32(ent, &largo)) return -1;
        if (!leer_u32(ent, &rango) || largo > MAX_CARGA || fread(carga, 1, largo, ent) != largo)
            return -1;
        if (largo == 0) continue;
        escribir_u32(sal, largo);
        escribir_u32(sal, rango);
        fwrite(carga, 1, largo, sal);
        n--;
    }
    return 0;
}

int main(int argc, char **argv) {
    FILE *ent, *sal;
    int buenos, perdidas, i;
    if (argc != 5 || (buenos = atoi(argv[2])) <= 0 || (perdidas = atoi(argv[3])) <= 0) {
        fprintf(stderr, "uso: secuencia_plc <entrada.bit> <buenos> <perdidas> <salida.bit>\n");
        return 1;
    }
    ent = fopen(argv[1], "rb");
    if (!ent) { perror(argv[1]); return 1; }
    sal = crear_salida(argv[4]);
    if (!sal) { perror(argv[4]); return 1; }
    if (copiar_paquetes(ent, sal, buenos) != 0) goto corto;
    for (i = 0; i < perdidas; i++) {
        escribir_u32(sal, 0);
        escribir_u32(sal, 0);
    }
    if (copiar_paquetes(ent, sal, buenos) != 0) goto corto;
    fclose(ent);
    if (fclose(sal) != 0) { perror(argv[4]); return 1; }
    printf("%s: %d paquetes, %d pérdidas\n", argv[4], 2 * buenos, perdidas);
    return 0;
corto:
    fprintf(stderr, "%s: faltan paquetes o hay un registro dañado\n", argv[1]);
    return 1;
}
