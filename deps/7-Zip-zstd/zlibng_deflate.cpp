// Deflate decoding for zip/7z through zlib-ng instead of 7-Zip's own decoder.
//
// Why: measured on a real No-Intro pack member (14.1 MB -> 49.6 MB), zlib-ng inflates at
// ~667 MB/s against flate2/miniz_oxide's ~407 and 7-Zip's own decoder's ~205 as seen through
// this library's extract path. Reading zipped ROMs is a hot path for the app that consumes
// this library, so the decoder is worth replacing; nothing else about 7-Zip changes.
//
// Scope, deliberately narrow:
//   * DECODING only. Compression still uses 7-Zip's DeflateEncoder -- that path is not hot
//     here, and swapping it would mean matching 7-Zip's method/level property handling.
//   * Method 0x40108 (Deflate) only. Deflate64 (0x40109) keeps 7-Zip's decoder, which zlib
//     cannot do: 64 KB windows and the extended length code are outside the zlib format.
//
// `user/Makefile.sevenzip_lib` drops DeflateRegister.o and builds this instead, so exactly one
// codec claims 0x40108. DeflateDecoder.o stays in the build because Deflate64Register and
// ZlibDecoder both still use it.
//
// The interfaces are the ones CPP/7zip/Archive/Zip/ZipHandler.cpp actually asks the coder for
// (it QueryInterfaces ICompressSetFinishMode and ICompressGetInStreamProcessedSize); the
// read-ahead interfaces 7-Zip's own decoder also exposes are not implemented, because the zip
// handler does not request them and guessing at their contract would be worse than leaving
// them absent.

// Includes are relative to user/, matching sevenzip_lib.cpp -- this file is compiled from the
// Format7zF directory (see Makefile.sevenzip_lib's header) but lives here, so it cannot use
// the vendored sources' own StdAfx.h-relative style.
#include "CPP/Common/MyWindows.h"

#include "CPP/Common/MyCom.h"
#include "CPP/7zip/ICoder.h"
#include "CPP/7zip/Common/RegisterCodec.h"
#include "CPP/7zip/Common/StreamUtils.h"
#include "CPP/7zip/Compress/DeflateEncoder.h"

#include <zlib.h>

#include <vector>

namespace NCompress {
namespace NZlibNgDeflate {

// Big enough that the per-call overhead is irrelevant, small enough to stay cache-friendly.
static const size_t kInBufSize = 1 << 18;   // 256 KB
static const size_t kOutBufSize = 1 << 18;

class CDecoder:
  public ICompressCoder,
  public ICompressSetFinishMode,
  public ICompressGetInStreamProcessedSize,
  public CMyUnknownImp
{
  Z7_COM_QI_BEGIN2(ICompressCoder)
  Z7_COM_QI_ENTRY(ICompressSetFinishMode)
  Z7_COM_QI_ENTRY(ICompressGetInStreamProcessedSize)
  Z7_COM_QI_END
  Z7_COM_ADDREF_RELEASE

  Z7_IFACE_COM7_IMP(ICompressCoder)
  Z7_IFACE_COM7_IMP(ICompressSetFinishMode)
  Z7_IFACE_COM7_IMP(ICompressGetInStreamProcessedSize)

  bool _finishMode;
  UInt64 _inProcessed;
public:
  CDecoder(): _finishMode(false), _inProcessed(0) {}
  // Deleted through ICompressCoder* by CMyComPtr, so the destructor has to be virtual.
  virtual ~CDecoder() {}
};

Z7_COM7F_IMF(CDecoder::SetFinishMode(UInt32 finishMode))
{
  _finishMode = (finishMode != 0);
  return S_OK;
}

Z7_COM7F_IMF(CDecoder::GetInStreamProcessedSize(UInt64 *value))
{
  // Exactly the bytes inflate consumed, not the bytes read into the buffer. The zip handler
  // uses this to find where the entry's stream ended, so over-reporting read-ahead would
  // desynchronise it from the next local header.
  *value = _inProcessed;
  return S_OK;
}

Z7_COM7F_IMF(CDecoder::Code(ISequentialInStream *inStream, ISequentialOutStream *outStream,
    const UInt64 * /* inSize */, const UInt64 *outSize, ICompressProgressInfo *progress))
{
  _inProcessed = 0;

  z_stream zs;
  memset(&zs, 0, sizeof(zs));
  // Raw deflate: no zlib header, no adler -- what a zip entry stores.
  if (inflateInit2(&zs, -15) != Z_OK)
    return E_OUTOFMEMORY;

  CMyComPtr<ISequentialInStream> inKeep(inStream);
  CMyComPtr<ISequentialOutStream> outKeep(outStream);

  std::vector<Byte> inVec(kInBufSize);
  std::vector<Byte> outVec(kOutBufSize);
  Byte * const inBuf = inVec.data();
  Byte * const outBuf = outVec.data();

  HRESULT res = S_OK;
  UInt64 outProcessed = 0;
  bool finished = false;
  size_t avail = 0;
  size_t pos = 0;

  for (;;)
  {
    if (avail == 0)
    {
      size_t got = kInBufSize;
      res = ReadStream(inStream, inBuf, &got);
      if (res != S_OK)
        break;
      if (got == 0)
      {
        // Input exhausted. A truncated stream is only an error when the caller asked for a
        // complete one; otherwise 7-Zip treats what was produced as the result.
        if (!finished && _finishMode)
          res = S_FALSE;
        break;
      }
      avail = got;
      pos = 0;
    }

    zs.next_in = inBuf + pos;
    zs.avail_in = (uInt)avail;
    zs.next_out = outBuf;
    zs.avail_out = (uInt)kOutBufSize;

    const int zr = inflate(&zs, Z_NO_FLUSH);
    if (zr != Z_OK && zr != Z_STREAM_END && zr != Z_BUF_ERROR)
    {
      res = S_FALSE;
      break;
    }

    const size_t consumed = avail - (size_t)zs.avail_in;
    pos += consumed;
    avail -= consumed;
    _inProcessed += consumed;

    const size_t produced = kOutBufSize - (size_t)zs.avail_out;
    if (produced != 0)
    {
      res = WriteStream(outStream, outBuf, produced);
      if (res != S_OK)
        break;
      outProcessed += produced;
      if (progress)
      {
        res = progress->SetRatioInfo(&_inProcessed, &outProcessed);
        if (res != S_OK)
          break;
      }
    }

    if (zr == Z_STREAM_END)
    {
      finished = true;
      break;
    }
    // Neither side moved and there is nothing left to feed: a malformed stream rather than a
    // reason to spin.
    if (consumed == 0 && produced == 0 && avail == 0 && zr == Z_BUF_ERROR)
      continue;
    if (outSize && outProcessed >= *outSize)
    {
      finished = true;
      break;
    }
  }

  inflateEnd(&zs);
  return res;
}

REGISTER_CODEC_CREATE(CreateDec, CDecoder)

#if !defined(Z7_EXTRACT_ONLY) && !defined(Z7_DEFLATE_EXTRACT_ONLY)
REGISTER_CODEC_CREATE(CreateEnc, NDeflate::NEncoder::CCOMCoder)
#else
#define CreateEnc NULL
#endif

REGISTER_CODEC_2(Deflate, CreateDec, CreateEnc, 0x40108, "Deflate")

}}
