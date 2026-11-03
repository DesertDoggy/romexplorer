/* See rxaac.h. */
#include <stdlib.h>
#include <string.h>

#include "ixheaac_type_def.h"
#include "impd_drc_common_enc.h"
#include "impd_drc_uni_drc.h"
#include "impd_drc_tables.h"
#include "impd_drc_api.h"
#include "ixheaace_api.h"
#include "ixheaac_error_standards.h"

#include "rxaac.h"

/* Values libxaac's own testbench uses (test/encoder/ixheaace_testbench.c); the library has
 * no header for them. */
#define RXAAC_BITRES_SIZE_LC 768
#define RXAAC_METHOD_PROGRAM_LOUDNESS 1
#define RXAAC_MEASUREMENT_BS_1770_3 2
#define RXAAC_USAC_FREQUENCY_DOMAIN 1
#define RXAAC_CCFL_NO_SBR_1024 1
/* A frame a decoder can start from, about once a second. libxaac's default (-1) makes only
 * the first frame one, and a player then cannot seek: romexplorer lists these frames in the
 * file's sync table (src/rom/m4a.rs) and a seek lands on the nearest. */
#define RXAAC_RAP_INTERVAL_MS 1000

struct rxaac {
  ixheaace_user_config_struct cfg;
  int32_t channels;
  int32_t frame_samples;
  uint8_t *config;
  int32_t config_len;
};

/* libxaac asks for aligned blocks through these two and frees with the second. The block's
 * real start is kept just before the address handed out. */
static pVOID rx_alloc(UWORD32 size, UWORD32 alignment) {
  if (alignment < sizeof(void *)) alignment = (UWORD32)sizeof(void *);
  void *base = malloc((size_t)size + alignment + sizeof(void *));
  if (!base) return NULL;
  uintptr_t at = ((uintptr_t)base + sizeof(void *) + alignment - 1) & ~((uintptr_t)alignment - 1);
  ((void **)at)[-1] = base;
  return (pVOID)at;
}

static VOID rx_free(pVOID ptr) {
  if (ptr) free(((void **)ptr)[-1]);
}

RXAAC_API int32_t rxaac_abi_version(void) { return RXAAC_ABI_VERSION; }

RXAAC_API rxaac *rxaac_open(int32_t sample_rate, int32_t channels, int32_t bitrate, int32_t xhe, double loudness, int32_t *error) {
  if (error) *error = 0;
  rxaac *enc = (rxaac *)calloc(1, sizeof(*enc));
  if (!enc) return NULL;
  ixheaace_input_config *in = &enc->cfg.input_config;
  ixheaace_output_config *out = &enc->cfg.output_config;

  out->malloc_xheaace = rx_alloc;
  out->free_xheaace = rx_free;

  /* USAC always reads this struct and copies its own loudness information back into it;
   * zeroed means "no DRC of our own". Allocated with the library's allocator because its
   * error paths free it with that. */
  in->pv_drc_cfg = rx_alloc(sizeof(ia_drc_input_config), DEFAULT_MEM_ALIGN_8);
  if (!in->pv_drc_cfg) {
    free(enc);
    return NULL;
  }
  memset(in->pv_drc_cfg, 0, sizeof(ia_drc_input_config));

  in->ui_pcm_wd_sz = 16;
  in->i_bitrate = bitrate;
  in->frame_length = 1024;
  in->frame_cmd_flag = 0;
  in->aot = xhe ? AOT_USAC : AOT_AAC_LC;
  in->i_mps_tree_config = -1;
  /* No SBR: at the bitrates this is used for (64 kbps and up) the full-band core sounds
   * better, and one access unit then always covers exactly frame_samples. */
  in->esbr_flag = 0;
  in->user_esbr_flag = 1;
  in->i_channels = channels;
  in->i_samp_freq = (UWORD32)sample_rate;
  in->i_native_samp_freq = sample_rate;
  in->i_channels_mask = 0;
  in->i_num_coupling_chan = 0;
  in->i_use_mps = 0;
  in->i_use_adts = 0;
  in->i_use_es = 1;
  in->usac_en = xhe ? 1 : 0;
  in->codec_mode = RXAAC_USAC_FREQUENCY_DOMAIN;
  in->cplx_pred = 0;
  in->ccfl_idx = RXAAC_CCFL_NO_SBR_1024;
  in->use_drc_element = 0;
  in->random_access_interval = RXAAC_RAP_INTERVAL_MS;
  in->method_def = RXAAC_METHOD_PROGRAM_LOUDNESS;
  in->measured_loudness = loudness;
  in->measurement_system = RXAAC_MEASUREMENT_BS_1770_3;
  in->sample_peak_level = 0.0f;
  in->use_delay_adjustment = 1;
  in->user_tns_flag = 1;

  in->aac_config.bitrate = bitrate;
  in->aac_config.sample_rate = sample_rate;
  in->aac_config.num_channels_in = channels;
  in->aac_config.num_channels_out = channels;
  in->aac_config.use_tns = 1;
  in->aac_config.inv_quant = 2;
  in->aac_config.bitreservoir_size = RXAAC_BITRES_SIZE_LC;

  IA_ERRORCODE err = ixheaace_create((pVOID)in, (pVOID)out);
  if (err & IA_FATAL_ERROR) {
    if (error) *error = (int32_t)err;
    ixheaace_delete((pVOID)out);
    rx_free(in->pv_drc_cfg);
    free(enc);
    return NULL;
  }

  enc->channels = channels;
  enc->frame_samples = out->input_size / (channels * 2);
  enc->config_len = out->i_out_bytes > 0 ? out->i_out_bytes : 0;
  if (enc->config_len > 0) {
    enc->config = (uint8_t *)malloc((size_t)enc->config_len);
    if (enc->config) {
      memcpy(enc->config, out->mem_info_table[IA_MEMTYPE_OUTPUT].mem_ptr, (size_t)enc->config_len);
    } else {
      enc->config_len = 0;
    }
  }
  return enc;
}

RXAAC_API int32_t rxaac_frame_samples(const rxaac *enc) { return enc ? enc->frame_samples : 0; }

RXAAC_API const uint8_t *rxaac_config(const rxaac *enc, int32_t *len) {
  if (len) *len = enc ? enc->config_len : 0;
  return enc ? enc->config : NULL;
}

RXAAC_API int32_t rxaac_encode(rxaac *enc, const int16_t *pcm, const uint8_t **out) {
  if (out) *out = NULL;
  if (!enc || !pcm) return -1;
  ixheaace_output_config *oc = &enc->cfg.output_config;
  memcpy(oc->mem_info_table[IA_MEMTYPE_INPUT].mem_ptr, pcm, (size_t)enc->frame_samples * (size_t)enc->channels * 2);
  IA_ERRORCODE err = ixheaace_process(oc->pv_ia_process_api_obj, (pVOID)&enc->cfg.input_config, (pVOID)oc);
  if (err & IA_FATAL_ERROR) return err < 0 ? (int32_t)err : -(int32_t)(err & 0x7fffffff) - 1;
  if (out) *out = (const uint8_t *)oc->mem_info_table[IA_MEMTYPE_OUTPUT].mem_ptr;
  return oc->i_out_bytes > 0 ? oc->i_out_bytes : 0;
}

RXAAC_API void rxaac_close(rxaac *enc) {
  if (!enc) return;
  ixheaace_delete((pVOID)&enc->cfg.output_config);
  rx_free(enc->cfg.input_config.pv_drc_cfg);
  free(enc->config);
  free(enc);
}
