// See shim/openssl/openssl_min.h for why this exists. SHA-1 is implemented here (FIPS 180-4);
// SHA-256 is ZArchive's sha_256.c.
#include "openssl/openssl_min.h"

#include <cstdint>
#include <cstring>
#include <new>

extern "C" {
#include "sha_256.h"
}

namespace {

struct Sha1
{
	uint32_t h[5];
	uint64_t length; // bytes
	uint8_t block[64];
	size_t used;

	void init()
	{
		h[0] = 0x67452301; h[1] = 0xEFCDAB89; h[2] = 0x98BADCFE; h[3] = 0x10325476; h[4] = 0xC3D2E1F0;
		length = 0;
		used = 0;
	}

	static uint32_t rol(uint32_t v, int n) { return (v << n) | (v >> (32 - n)); }

	void compress(const uint8_t* p)
	{
		uint32_t w[80];
		for (int i = 0; i < 16; i++)
			w[i] = (uint32_t)p[i * 4] << 24 | (uint32_t)p[i * 4 + 1] << 16 | (uint32_t)p[i * 4 + 2] << 8 | p[i * 4 + 3];
		for (int i = 16; i < 80; i++)
			w[i] = rol(w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16], 1);
		uint32_t a = h[0], b = h[1], c = h[2], d = h[3], e = h[4];
		for (int i = 0; i < 80; i++)
		{
			uint32_t f, k;
			if (i < 20) { f = (b & c) | (~b & d); k = 0x5A827999; }
			else if (i < 40) { f = b ^ c ^ d; k = 0x6ED9EBA1; }
			else if (i < 60) { f = (b & c) | (b & d) | (c & d); k = 0x8F1BBCDC; }
			else { f = b ^ c ^ d; k = 0xCA62C1D6; }
			uint32_t t = rol(a, 5) + f + e + k + w[i];
			e = d; d = c; c = rol(b, 30); b = a; a = t;
		}
		h[0] += a; h[1] += b; h[2] += c; h[3] += d; h[4] += e;
	}

	void update(const uint8_t* p, size_t n)
	{
		length += n;
		while (n > 0)
		{
			size_t take = 64 - used;
			if (take > n)
				take = n;
			std::memcpy(block + used, p, take);
			used += take;
			p += take;
			n -= take;
			if (used == 64)
			{
				compress(block);
				used = 0;
			}
		}
	}

	void final(uint8_t out[20])
	{
		uint64_t bits = length * 8;
		uint8_t pad = 0x80;
		update(&pad, 1);
		uint8_t zero = 0;
		while (used != 56)
			update(&zero, 1);
		uint8_t len[8];
		for (int i = 0; i < 8; i++)
			len[i] = (uint8_t)(bits >> (56 - 8 * i));
		update(len, 8);
		for (int i = 0; i < 5; i++)
		{
			out[i * 4] = (uint8_t)(h[i] >> 24);
			out[i * 4 + 1] = (uint8_t)(h[i] >> 16);
			out[i * 4 + 2] = (uint8_t)(h[i] >> 8);
			out[i * 4 + 3] = (uint8_t)h[i];
		}
	}
};

enum class Algo { None, Sha1, Sha256 };

} // namespace

struct evp_md_st
{
	Algo algo;
};

struct evp_md_ctx_st
{
	Algo algo = Algo::None;
	Sha1 sha1;
	Sha_256 sha256;
	uint8_t sha256Out[SIZE_OF_SHA_256_HASH];
};

static const evp_md_st g_sha1{Algo::Sha1};
static const evp_md_st g_sha256{Algo::Sha256};

extern "C" {

unsigned char* SHA1(const unsigned char* d, size_t n, unsigned char* md)
{
	Sha1 s;
	s.init();
	s.update(d, n);
	s.final(md);
	return md;
}

unsigned char* SHA256(const unsigned char* d, size_t n, unsigned char* md)
{
	calc_sha_256(md, d, n);
	return md;
}

EVP_MD_CTX* EVP_MD_CTX_new(void) { return new (std::nothrow) evp_md_ctx_st(); }
void EVP_MD_CTX_free(EVP_MD_CTX* ctx) { delete ctx; }
const EVP_MD* EVP_sha1(void) { return &g_sha1; }
const EVP_MD* EVP_sha256(void) { return &g_sha256; }

int EVP_DigestInit_ex(EVP_MD_CTX* ctx, const EVP_MD* type, ENGINE*)
{
	if (!ctx || !type)
		return 0;
	ctx->algo = type->algo;
	if (ctx->algo == Algo::Sha1)
		ctx->sha1.init();
	else
		sha_256_init(&ctx->sha256, ctx->sha256Out);
	return 1;
}

int EVP_DigestInit(EVP_MD_CTX* ctx, const EVP_MD* type) { return EVP_DigestInit_ex(ctx, type, nullptr); }

int EVP_DigestUpdate(EVP_MD_CTX* ctx, const void* d, size_t cnt)
{
	if (!ctx)
		return 0;
	if (ctx->algo == Algo::Sha1)
		ctx->sha1.update((const uint8_t*)d, cnt);
	else if (ctx->algo == Algo::Sha256)
		sha_256_write(&ctx->sha256, d, cnt);
	else
		return 0;
	return 1;
}

int EVP_DigestFinal_ex(EVP_MD_CTX* ctx, unsigned char* md, unsigned int* s)
{
	if (!ctx)
		return 0;
	if (ctx->algo == Algo::Sha1)
	{
		ctx->sha1.final(md);
		if (s)
			*s = SHA_DIGEST_LENGTH;
	}
	else if (ctx->algo == Algo::Sha256)
	{
		sha_256_close(&ctx->sha256);
		std::memcpy(md, ctx->sha256Out, SHA256_DIGEST_LENGTH);
		if (s)
			*s = SHA256_DIGEST_LENGTH;
	}
	else
		return 0;
	ctx->algo = Algo::None;
	return 1;
}

// --- not supported: console certificates and personalised tickets only -------------------
BN_CTX* BN_CTX_new(void) { return nullptr; }
void BN_CTX_free(BN_CTX*) {}
BIGNUM* BN_new(void) { return nullptr; }
void BN_free(BIGNUM*) {}
BIGNUM* BN_bin2bn(const unsigned char*, int, BIGNUM*) { return nullptr; }
int BN_bn2binpad(const BIGNUM*, unsigned char*, int) { return -1; }
int BN_num_bytes(const BIGNUM*) { return 0; }
EC_GROUP* EC_GROUP_new_by_curve_name(int) { return nullptr; }
void EC_GROUP_free(EC_GROUP*) {}
EC_KEY* EC_KEY_new_by_curve_name(int) { return nullptr; }
void EC_KEY_free(EC_KEY*) {}
int EC_KEY_generate_key(EC_KEY*) { return 0; }
const BIGNUM* EC_KEY_get0_private_key(const EC_KEY*) { return nullptr; }
int EC_KEY_set_private_key(EC_KEY*, const BIGNUM*) { return 0; }
int EC_KEY_set_public_key_affine_coordinates(EC_KEY*, BIGNUM*, BIGNUM*) { return 0; }
EC_POINT* EC_POINT_new(const EC_GROUP*) { return nullptr; }
void EC_POINT_free(EC_POINT*) {}
int EC_POINT_get_affine_coordinates(const EC_GROUP*, const EC_POINT*, BIGNUM*, BIGNUM*, BN_CTX*) { return 0; }
int EC_POINT_set_affine_coordinates(const EC_GROUP*, EC_POINT*, const BIGNUM*, const BIGNUM*, BN_CTX*) { return 0; }
int EC_POINT_mul(const EC_GROUP*, EC_POINT*, const BIGNUM*, const EC_POINT*, const BIGNUM*, BN_CTX*) { return 0; }
ECDSA_SIG* ECDSA_SIG_new(void) { return nullptr; }
void ECDSA_SIG_free(ECDSA_SIG*) {}
void ECDSA_SIG_get0(const ECDSA_SIG*, const BIGNUM** pr, const BIGNUM** ps)
{
	if (pr)
		*pr = nullptr;
	if (ps)
		*ps = nullptr;
}
int ECDSA_SIG_set0(ECDSA_SIG*, BIGNUM*, BIGNUM*) { return 0; }
ECDSA_SIG* ECDSA_do_sign(const unsigned char*, int, EC_KEY*) { return nullptr; }
int ECDSA_do_verify(const unsigned char*, int, const ECDSA_SIG*, EC_KEY*) { return -1; }
int ECDH_compute_key(void*, size_t, const EC_POINT*, const EC_KEY*, void* (*)(const void*, size_t, void*, size_t*)) { return -1; }

} // extern "C"
