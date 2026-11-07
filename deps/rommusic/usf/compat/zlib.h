/* lazyusf2 includes <zlib.h> for adler32() alone, which ../adler32.c provides. This stands in
 * for the real header so the build does not depend on zlib's development files being present
 * for the target (a cross toolchain usually has none). */
#ifndef ROMMUSIC_USF_ZLIB_H
#define ROMMUSIC_USF_ZLIB_H
unsigned long adler32(unsigned long adler, const unsigned char *buf, unsigned int len);
#endif
