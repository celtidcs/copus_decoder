/* Cuenta, en un flujo .bit de opus_demo, los paquetes con LBRR (opus_packet_has_lbrr) y las
   pérdidas seguidas de un paquete con LBRR, que opus_demo recupera con FEC. Sirve para comprobar
   las cuentas de LIBOPUS_LBRR_COUNTS en tests/referencia_libopus.rs.
   Uso: lbrr <flujo.bit> */
#include <stdio.h>
#include <stdlib.h>
#include "opus.h"

static int leer_u32(FILE *f, unsigned *v) {
   unsigned char b[4];
   if (fread(b, 1, 4, f) != 4) return 0;
   *v = ((unsigned)b[0] << 24) | ((unsigned)b[1] << 16) | ((unsigned)b[2] << 8) | b[3];
   return 1;
}

int main(int argc, char **argv) {
   FILE *f;
   if (argc != 2) { fprintf(stderr, "uso: lbrr <flujo.bit>\n"); return 1; }
   f = fopen(argv[1], "rb");
   unsigned largo, rango;
   static unsigned char carga[1500];
   long total = 0, con_lbrr = 0, perdidas = 0, recuperables = 0;
   int anterior_perdido = 0;
   if (!f) return 1;
   while (leer_u32(f, &largo) && leer_u32(f, &rango)) {
      int lbrr = 0;
      if (largo > sizeof carga || fread(carga, 1, largo, f) != largo) return 2;
      total++;
      if (largo == 0) { perdidas++; anterior_perdido = 1; continue; }
      lbrr = opus_packet_has_lbrr(carga, (opus_int32)largo) > 0;
      con_lbrr += lbrr;
      if (anterior_perdido && lbrr) recuperables++;
      anterior_perdido = 0;
   }
   printf("%s paquetes=%ld con_lbrr=%ld perdidas=%ld perdidas_recuperables_con_fec=%ld\n",
          argv[1], total, con_lbrr, perdidas, recuperables);
   return 0;
}
