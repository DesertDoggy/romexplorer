// cemuwua's stand-in for the parts of OpenSSL Cemu's ncrypto.cpp and FST.cpp call.
//
// Why not link OpenSSL: the only things the disc-to-WUA path uses are SHA-1 and SHA-256
// (FST block hashes). The rest of ncrypto.cpp -- ECDSA/ECDH for console certificates and
// personalised tickets -- is compiled in but never reached, and a real libcrypto would be one
// more shared library to deploy beside cemuwua (on Windows, where DLL dependencies are not
// looked up beside the loading DLL, a fragile one). So the digests are implemented here
// (SHA-256 through ZArchive's sha_256.c, which the library links anyway) and every ECC/BIGNUM
// call fails cleanly: NULL, 0 or -1, as OpenSSL does on error.
#pragma once
#include <stddef.h>

#define SHA_DIGEST_LENGTH 20
#define SHA256_DIGEST_LENGTH 32
#define NID_sect233r1 727

#ifdef __cplusplus
extern "C" {
#endif

typedef struct engine_st ENGINE;
typedef struct evp_md_st EVP_MD;
typedef struct evp_md_ctx_st EVP_MD_CTX;
typedef struct bignum_st BIGNUM;
typedef struct bignum_ctx BN_CTX;
typedef struct ec_group_st EC_GROUP;
typedef struct ec_key_st EC_KEY;
typedef struct ec_point_st EC_POINT;
typedef struct ECDSA_SIG_st ECDSA_SIG;

unsigned char* SHA1(const unsigned char* d, size_t n, unsigned char* md);
unsigned char* SHA256(const unsigned char* d, size_t n, unsigned char* md);

EVP_MD_CTX* EVP_MD_CTX_new(void);
void EVP_MD_CTX_free(EVP_MD_CTX* ctx);
const EVP_MD* EVP_sha1(void);
const EVP_MD* EVP_sha256(void);
int EVP_DigestInit(EVP_MD_CTX* ctx, const EVP_MD* type);
int EVP_DigestInit_ex(EVP_MD_CTX* ctx, const EVP_MD* type, ENGINE* impl);
int EVP_DigestUpdate(EVP_MD_CTX* ctx, const void* d, size_t cnt);
int EVP_DigestFinal_ex(EVP_MD_CTX* ctx, unsigned char* md, unsigned int* s);

BN_CTX* BN_CTX_new(void);
void BN_CTX_free(BN_CTX* c);
BIGNUM* BN_new(void);
void BN_free(BIGNUM* a);
BIGNUM* BN_bin2bn(const unsigned char* s, int len, BIGNUM* ret);
int BN_bn2binpad(const BIGNUM* a, unsigned char* to, int tolen);
int BN_num_bytes(const BIGNUM* a);

EC_GROUP* EC_GROUP_new_by_curve_name(int nid);
void EC_GROUP_free(EC_GROUP* group);
EC_KEY* EC_KEY_new_by_curve_name(int nid);
void EC_KEY_free(EC_KEY* key);
int EC_KEY_generate_key(EC_KEY* key);
const BIGNUM* EC_KEY_get0_private_key(const EC_KEY* key);
int EC_KEY_set_private_key(EC_KEY* key, const BIGNUM* prv);
int EC_KEY_set_public_key_affine_coordinates(EC_KEY* key, BIGNUM* x, BIGNUM* y);
EC_POINT* EC_POINT_new(const EC_GROUP* group);
void EC_POINT_free(EC_POINT* point);
int EC_POINT_get_affine_coordinates(const EC_GROUP* group, const EC_POINT* p, BIGNUM* x, BIGNUM* y, BN_CTX* ctx);
int EC_POINT_set_affine_coordinates(const EC_GROUP* group, EC_POINT* p, const BIGNUM* x, const BIGNUM* y, BN_CTX* ctx);
int EC_POINT_mul(const EC_GROUP* group, EC_POINT* r, const BIGNUM* n, const EC_POINT* q, const BIGNUM* m, BN_CTX* ctx);
ECDSA_SIG* ECDSA_SIG_new(void);
void ECDSA_SIG_free(ECDSA_SIG* sig);
void ECDSA_SIG_get0(const ECDSA_SIG* sig, const BIGNUM** pr, const BIGNUM** ps);
int ECDSA_SIG_set0(ECDSA_SIG* sig, BIGNUM* r, BIGNUM* s);
ECDSA_SIG* ECDSA_do_sign(const unsigned char* dgst, int dgst_len, EC_KEY* eckey);
int ECDSA_do_verify(const unsigned char* dgst, int dgst_len, const ECDSA_SIG* sig, EC_KEY* eckey);
int ECDH_compute_key(void* out, size_t outlen, const EC_POINT* pub_key, const EC_KEY* ecdh,
                     void* (*KDF)(const void* in, size_t inlen, void* out, size_t* outlen));

#ifdef __cplusplus
}
#endif
