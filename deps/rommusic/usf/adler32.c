/* lazyusf2's interpreters hash code blocks with zlib's adler32, and nothing else of zlib is
 * needed, so this is that one function rather than a zlib link. */
#include <stddef.h>

unsigned long adler32(unsigned long adler, const unsigned char *buf, unsigned int len) {
    unsigned long a = adler & 0xffff, b = (adler >> 16) & 0xffff;
    if (!buf) {
        return 1;
    }
    while (len--) {
        a = (a + *buf++) % 65521;
        b = (b + a) % 65521;
    }
    return (b << 16) | a;
}
