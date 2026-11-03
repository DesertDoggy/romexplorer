// sevenzip_lib.cpp -- implementation of sevenzip_lib.h.
//
// Adapted from CPP/7zip/UI/Client7z/Client7z.cpp (7-Zip's own minimal reference client
// for driving IInArchive/IOutArchive), with three structural differences:
//   1. Client7z dynamically loads 7z.dll/7z.so at runtime and gets CreateObject via
//      GetProcAddress, looking up a handler by CLSID. We're statically linking the
//      Zip/7z handler code directly into this same library (see Makefile.sevenzip_lib),
//      so we skip that whole indirection and just `new NArchive::N7z::CHandler()` /
//      `new NArchive::NZip::CHandler()` directly -- both classes are literally named
//      CHandler in their own namespace, and the format is already known from our own
//      SevenZipFormat enum at the call site.
//   2. Client7z is a CLI (parses argv, prints to stdout). This exposes the same
//      underlying operations (open/list/extract/create) as flat C functions with a
//      caller-supplied progress callback instead.
//   3. No password/encryption support in either direction -- CryptoGetTextPassword
//      always fails, matching Client7z's own default (`#else PrintError(...); return
//      E_ABORT;`) rather than its commented-out "ask the user" branch.

#define SEVENZIP_BUILDING_DLL
#include "sevenzip_lib.h"

#include "CPP/Common/MyWindows.h"
#include "CPP/Common/MyInitGuid.h"

#include "CPP/Common/Defs.h"
#include "CPP/Common/IntToString.h"
#include "CPP/Common/StringConvert.h"
#include "CPP/Common/UTFConvert.h"

#include "CPP/Windows/FileDir.h"
#include "CPP/Windows/FileFind.h"
#include "CPP/Windows/FileName.h"
#include "CPP/Windows/PropVariant.h"
#include "CPP/Windows/PropVariantConv.h"

#include "CPP/7zip/Common/FileStreams.h"
#include "CPP/7zip/Archive/IArchive.h"
#include "CPP/7zip/IPassword.h"
#include "CPP/7zip/Common/RegisterArc.h"

#include "CPP/7zip/Archive/7z/7zHandler.h"
#include "CPP/7zip/Archive/Zip/ZipHandler.h"

#include <string>
#include <vector>

#ifdef SEVENZIP_WITH_STREAMING
#include <condition_variable>
#include <ctime>
#include <deque>
#include <mutex>
#include <thread>
#endif

using namespace NWindows;
using namespace NFile;
using namespace NDir;

#ifdef _WIN32
// CPP/Windows/DLL.cpp (part of WIN_OBJS) declares this `extern` and expects whichever
// program links it in to define it -- every normal 7-Zip entry point does (Client7z.cpp,
// Console/Main.cpp, the various DllExports*.cpp) and we're no exception. NULL is what
// Client7z.cpp itself uses; nothing here relies on it pointing at a real module.
extern HINSTANCE g_hInstance;
HINSTANCE g_hInstance = NULL;
#endif

// 7zRegister.cpp/ZipRegister.cpp each call RegisterArc() from a static initializer to
// add themselves to a global CLSID-lookup table (whose real home is
// CPP/7zip/Archive/ArchiveExports.cpp, which this library deliberately does not build
// -- see NewInHandler/NewOutHandler below: we construct handler classes directly
// instead of going through that CLSID/CreateObject lookup). The call itself is
// harmless to no-op: nothing in this library ever reads that table.
void RegisterArc(const CArcInfo *) throw() {}

// ---------------------------------------------------------------------------
// Error reporting: one message per calling thread, matching sevenzip_get_last_error's
// documented "most recent failed call on this thread" contract.
// ---------------------------------------------------------------------------

static thread_local std::string g_lastError;

static void SetLastError(const char *msg)
{
    g_lastError = msg ? msg : "";
}

static void SetLastErrorHR(const char *context, HRESULT hr)
{
    char buf[256];
    ConvertUInt32ToString((UInt32)hr, buf); // hex would be nicer, decimal is fine here
    g_lastError = std::string(context) + " (HRESULT=" + buf + ")";
}

const char *sevenzip_get_last_error(void)
{
    return g_lastError.c_str();
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

static std::string UStringToUtf8(const UString &s)
{
    AString a;
    ConvertUnicodeToUTF8(s, a);
    return std::string(a.Ptr(), a.Len());
}

static UString Utf8ToUString(const char *s)
{
    AString a(s);
    UString u;
    ConvertUTF8ToUnicode(a, u);
    return u;
}

// Mirrors CArcTime::Set_From_Prop from Client7z.cpp -- extracts full-precision time
// (the 100ns FILETIME plus 7-Zip's own extra sub-tick nanosecond field) straight from
// a kpidMTime property, without needing the rest of that class.
static void FileTimeFromProp(const PROPVARIANT &prop, SevenZipFileTime &out)
{
    out.filetime_100ns = 0;
    out.extra_ns = 0;
    out.is_defined = 0;

    if (prop.vt != VT_FILETIME)
        return;

    out.filetime_100ns = ((UInt64)(UInt32)prop.filetime.dwHighDateTime << 32) |
                          (UInt32)prop.filetime.dwLowDateTime;
    out.is_defined = 1;

    const unsigned prec = prop.wReserved1;
    if (prec != 0 && prec <= k_PropVar_TimePrec_1ns && prop.wReserved3 == 0)
    {
        const unsigned ns100 = prop.wReserved2;
        if (ns100 < 100)
            out.extra_ns = (uint8_t)ns100;
    }
}

static bool GetBoolProp(IInArchive *archive, UInt32 index, PROPID propID)
{
    NCOM::CPropVariant prop;
    if (archive->GetProperty(index, propID, &prop) != S_OK)
        return false;
    if (prop.vt == VT_BOOL)
        return VARIANT_BOOLToBool(prop.boolVal);
    return false;
}

static UInt64 GetUInt64Prop(IInArchive *archive, UInt32 index, PROPID propID)
{
    NCOM::CPropVariant prop;
    if (archive->GetProperty(index, propID, &prop) != S_OK)
        return 0;
    UInt64 v = 0;
    ConvertPropVariantToUInt64(prop, v);
    return v;
}

static const wchar_t *MethodName(SevenZipMethod method)
{
    switch (method)
    {
        case SEVENZIP_METHOD_COPY: return L"Copy";
        case SEVENZIP_METHOD_DEFLATE: return L"Deflate";
        case SEVENZIP_METHOD_DEFLATE64: return L"Deflate64";
        case SEVENZIP_METHOD_BZIP2: return L"BZip2";
        case SEVENZIP_METHOD_LZMA: return L"LZMA";
        case SEVENZIP_METHOD_LZMA2: return L"LZMA2";
        case SEVENZIP_METHOD_ZSTD: return L"ZSTD";
        default: return L"LZMA2";
    }
}

// ---------------------------------------------------------------------------
// SevenZipArchive: the opaque handle. Entries are cached eagerly at open time so
// sevenzip_get_entry's `path` pointer (owned by the handle, per the header's contract)
// stays valid for the handle's whole lifetime without re-querying the archive.
// ---------------------------------------------------------------------------

struct CCachedEntry
{
    std::string Path;
    UInt64 Size;
    UInt64 PackSize;
    UInt32 Attrib;
    SevenZipFileTime MTime;
    bool IsDir;
};

struct SevenZipArchive
{
    CMyComPtr<IInArchive> Archive;
    std::vector<CCachedEntry> Entries;
};

// ---------------------------------------------------------------------------
// Open callback -- no password support: any CryptoGetTextPassword call fails the open.
// ---------------------------------------------------------------------------

class CArchiveOpenCallback Z7_final:
    public IArchiveOpenCallback,
    public ICryptoGetTextPassword,
    public CMyUnknownImp
{
    Z7_IFACES_IMP_UNK_2(IArchiveOpenCallback, ICryptoGetTextPassword)
public:
    SevenZipProgressCb OnProgress;
    void *UserData;

    CArchiveOpenCallback(SevenZipProgressCb onProgress, void *userData):
        OnProgress(onProgress), UserData(userData) {}
};

Z7_COM7F_IMF(CArchiveOpenCallback::SetTotal(const UInt64 *, const UInt64 *))
{
    return S_OK;
}

Z7_COM7F_IMF(CArchiveOpenCallback::SetCompleted(const UInt64 *, const UInt64 *))
{
    if (OnProgress)
    {
        if (!OnProgress("Opening archive", -1.0f, UserData))
            return E_ABORT;
    }
    return S_OK;
}

Z7_COM7F_IMF(CArchiveOpenCallback::CryptoGetTextPassword(BSTR *))
{
    SetLastError("Archive is password-protected; encryption is not supported");
    return E_ABORT;
}

// ---------------------------------------------------------------------------
// A growable in-memory ISequentialOutStream, for sevenzip_extract_entry_to_buffer.
// ---------------------------------------------------------------------------

class CMemOutStream Z7_final: public ISequentialOutStream, public CMyUnknownImp
{
    Z7_IFACES_IMP_UNK_1(ISequentialOutStream)
public:
    std::vector<uint8_t> Buf;
};

Z7_COM7F_IMF(CMemOutStream::Write(const void *data, UInt32 size, UInt32 *processedSize))
{
    if (size != 0)
    {
        const size_t oldLen = Buf.size();
        Buf.resize(oldLen + size);
        memcpy(Buf.data() + oldLen, data, size);
    }
    if (processedSize)
        *processedSize = size;
    return S_OK;
}

#ifdef SEVENZIP_WITH_STREAMING

// ---------------------------------------------------------------------------
// Pass-through ISequentialOutStream that hands each decompressed chunk to a C
// callback as 7-Zip produces it, optionally forwarding the same bytes to a real
// inner stream (the output file). With no inner stream nothing reaches disk: the
// bytes are seen by the callback and then dropped.
//
// Same wrap-and-observe shape as 7-Zip's own COutStreamWithCRC
// (CPP/7zip/Archive/Common/OutStreamWithCRC.h), and it honors the same partial-write
// contract (IStream.h: *processedSize <= size): the inner stream is written first and
// only the bytes it actually accepted are handed to the callback, so the callback
// never sees bytes that failed to reach disk.
//
// Deliberately implemented here rather than via the IArchiveExtractCallbackData2
// hook in CPP/7zip/Archive/IArchive.h: that hook is wired into the 7z handler only
// (CPP/7zip/Archive/7z/7zExtract.cpp), whereas wrapping the stream this callback
// already hands out works identically for 7z and zip and touches no vendored code.
// ---------------------------------------------------------------------------

class CTapOutStream Z7_final: public ISequentialOutStream, public CMyUnknownImp
{
    Z7_IFACES_IMP_UNK_1(ISequentialOutStream)
public:
    /// Real sink for the bytes, or null to discard them after the callback sees them.
    CMyComPtr<ISequentialOutStream> Inner;
    SevenZipDataCb OnData;
    void *UserData;
    /// Archive entry these chunks belong to, so the consumer can route them per file.
    UInt32 EntryIndex;
    /// Running offset within this entry.
    UInt64 Offset;
    /// Points at the owning CArchiveExtractCallback's flag, which outlives this stream
    /// (this object dies when the callback releases its CMyComPtr, so the "did the
    /// callback ask to stop" answer cannot live here).
    bool *StopFlag;

    CTapOutStream():
        OnData(nullptr), UserData(nullptr), EntryIndex(0), Offset(0), StopFlag(nullptr) {}
};

Z7_COM7F_IMF(CTapOutStream::Write(const void *data, UInt32 size, UInt32 *processedSize))
{
    if (processedSize)
        *processedSize = 0;

    UInt32 written = size;
    if (Inner)
    {
        written = 0;
        RINOK(Inner->Write(data, size, &written))
    }

    if (written != 0 && OnData)
    {
        if (!OnData(EntryIndex, Offset, data, written, UserData))
        {
            if (StopFlag)
                *StopFlag = true;
            return E_ABORT;
        }
    }

    Offset += written;
    if (processedSize)
        *processedSize = written;
    return S_OK;
}

#endif // SEVENZIP_WITH_STREAMING

// ---------------------------------------------------------------------------
// Extract callback -- always targets exactly one archive index (the caller-requested
// one), writing either to a fixed output file path or into a CMemOutStream, selected
// by ToBuffer. Unlike Client7z's directory-tree version, GetStream never needs to
// check "is this the right item" since Extract() below is only ever asked for the one
// index we want.
// ---------------------------------------------------------------------------

class CArchiveExtractCallback Z7_final:
    public IArchiveExtractCallback,
    public ICryptoGetTextPassword,
    public CMyUnknownImp
{
    Z7_IFACES_IMP_UNK_2(IArchiveExtractCallback, ICryptoGetTextPassword)
    Z7_IFACE_COM7_IMP(IProgress)
public:
    bool ToBuffer;
    FString OutFilePath;             // used when !ToBuffer
    CMemOutStream *MemStreamSpec;    // used when ToBuffer; kept alive by MemStreamOwner below
    // Owning reference to *MemStreamSpec for the whole extraction.
    //
    // Without it the buffer is read after it has been freed. SetOperationResult() calls
    // _outStream.Release(), and 7-Zip then drops its own reference, so the CMemOutStream --
    // and the std::vector holding every decompressed byte -- is destroyed before
    // ExtractOne() copies Buf out. Small entries appeared to work only because glibc keeps
    // sub-128KB allocations mapped after free; anything larger is mmap'd, munmap'd on free,
    // and segfaults on the memcpy (reproduced at 4.8 MB, fine at 2 KB).
    //
    // This reference is held by the callback object itself, so it outlives
    // SetOperationResult() and dies with the callback -- after the copy.
    CMyComPtr<ISequentialOutStream> MemStreamOwner;
    UInt64 Total;
    SevenZipProgressCb OnProgress;
    void *UserData;
    bool Cancelled;
    bool Failed;

    COutFileStream *_outFileStreamSpec;
    CMyComPtr<ISequentialOutStream> _outStream;

#ifdef SEVENZIP_WITH_STREAMING
    /// When set, every decompressed chunk is handed to this as 7-Zip produces it.
    SevenZipDataCb OnData;
    void *DataUserData;
    /// Suppress the output file entirely -- stream the bytes and discard them.
    bool StreamOnly;
    /// Set by CTapOutStream when OnData asked to stop (see CTapOutStream::StopFlag).
    bool DataStopped;
#endif

    // Initializer order follows declaration order (-Wreorder is an error here).
    CArchiveExtractCallback():
        ToBuffer(false), MemStreamSpec(nullptr), Total(0),
        OnProgress(nullptr), UserData(nullptr), Cancelled(false), Failed(false),
        _outFileStreamSpec(nullptr)
#ifdef SEVENZIP_WITH_STREAMING
        , OnData(nullptr), DataUserData(nullptr), StreamOnly(false), DataStopped(false)
#endif
        {}
};

Z7_COM7F_IMF(CArchiveExtractCallback::SetTotal(UInt64 size))
{
    Total = size;
    return S_OK;
}

Z7_COM7F_IMF(CArchiveExtractCallback::SetCompleted(const UInt64 *completeValue))
{
    if (OnProgress)
    {
        float percent = -1.0f;
        if (completeValue && Total > 0)
            percent = (float)((double)*completeValue * 100.0 / (double)Total);
        if (!OnProgress("Extracting", percent, UserData))
        {
            Cancelled = true;
            return E_ABORT;
        }
    }
    return S_OK;
}

Z7_COM7F_IMF(CArchiveExtractCallback::GetStream(UInt32 index,
    ISequentialOutStream **outStream, Int32 askExtractMode))
{
    (void)index; // only the streaming build routes chunks per entry
    *outStream = nullptr;
    _outStream.Release();

    if (askExtractMode != NArchive::NExtract::NAskMode::kExtract)
        return S_OK;

#ifdef SEVENZIP_WITH_STREAMING
    if (StreamOnly && !ToBuffer)
    {
        // No output file at all: the tap is the only sink, so bytes reach OnData and
        // are then dropped.
        CTapOutStream *tapSpec = new CTapOutStream();
        CMyComPtr<ISequentialOutStream> tapLoc(tapSpec);
        tapSpec->OnData = OnData;
        tapSpec->UserData = DataUserData;
        tapSpec->EntryIndex = index;
        tapSpec->StopFlag = &DataStopped;
        _outStream = tapLoc;
        *outStream = tapLoc.Detach();
        return S_OK;
    }
#endif

    if (ToBuffer)
    {
        MemStreamSpec = new CMemOutStream();
        CMyComPtr<ISequentialOutStream> streamLoc(MemStreamSpec);
        MemStreamOwner = streamLoc;   // survives SetOperationResult's _outStream.Release()
        _outStream = streamLoc;
        *outStream = streamLoc.Detach();
        return S_OK;
    }

    {
        int slashPos = -1;
        for (unsigned i = 0; i < OutFilePath.Len(); i++)
            if (IS_PATH_SEPAR(OutFilePath[i]))
                slashPos = (int)i;
        if (slashPos >= 0)
            CreateComplexDir(OutFilePath.Left(slashPos));
    }

    NFind::CFileInfo fi;
    if (fi.Find(OutFilePath))
        DeleteFileAlways(OutFilePath);

    _outFileStreamSpec = new COutFileStream;
    CMyComPtr<ISequentialOutStream> streamLoc(_outFileStreamSpec);
    if (!_outFileStreamSpec->Create_ALWAYS(OutFilePath))
    {
        SetLastError("Cannot create output file");
        return E_ABORT;
    }

#ifdef SEVENZIP_WITH_STREAMING
    if (OnData)
    {
        // One decompression pass feeds both the output file and the callback.
        CTapOutStream *tapSpec = new CTapOutStream();
        CMyComPtr<ISequentialOutStream> tapLoc(tapSpec);
        tapSpec->Inner = streamLoc;
        tapSpec->OnData = OnData;
        tapSpec->UserData = DataUserData;
        tapSpec->EntryIndex = index;
        tapSpec->StopFlag = &DataStopped;
        _outStream = tapLoc;
        *outStream = tapLoc.Detach();
        return S_OK;
    }
#endif

    _outStream = streamLoc;
    *outStream = streamLoc.Detach();
    return S_OK;
}

Z7_COM7F_IMF(CArchiveExtractCallback::PrepareOperation(Int32))
{
    return S_OK;
}

Z7_COM7F_IMF(CArchiveExtractCallback::SetOperationResult(Int32 operationResult))
{
    if (_outFileStreamSpec)
        RINOK(_outFileStreamSpec->Close())
    _outStream.Release();

    if (operationResult != NArchive::NExtract::NOperationResult::kOK)
    {
        Failed = true;
        switch (operationResult)
        {
            case NArchive::NExtract::NOperationResult::kUnsupportedMethod:
                SetLastError("Unsupported compression method"); break;
            case NArchive::NExtract::NOperationResult::kCRCError:
                SetLastError("CRC check failed"); break;
            case NArchive::NExtract::NOperationResult::kDataError:
                SetLastError("Data error"); break;
            default:
                SetLastError("Extraction failed"); break;
        }
    }
    return S_OK;
}

Z7_COM7F_IMF(CArchiveExtractCallback::CryptoGetTextPassword(BSTR *))
{
    SetLastError("Archive is password-protected; encryption is not supported");
    return E_ABORT;
}

// ---------------------------------------------------------------------------
// Update (create) callback -- adapted from Client7z's CArchiveUpdateCallback. Each
// item's data comes straight from a real filesystem file (input_paths[i]); the name
// recorded in the archive is archive_names[i], independent of the real path.
// ---------------------------------------------------------------------------

struct CDirItem
{
    UString PathForHandler;
    FString FullPath;
    NFind::CFileInfo Fi;
#ifdef SEVENZIP_WITH_STREAMING
    // Non-null: the member's bytes come from this caller source, not FullPath.
    const SevenZipSource *Source = nullptr;
#endif
};

#ifdef SEVENZIP_WITH_STREAMING
// ISequentialInStream over a SevenZipSource: its `read`, or `read_at` driven by a cursor.
// Never yields more than the source's declared size.
class CSourceInStream Z7_final:
    public ISequentialInStream,
    public CMyUnknownImp
{
    Z7_IFACES_IMP_UNK_1(ISequentialInStream)
public:
    const SevenZipSource *Source = nullptr;
    UInt64 Pos = 0;
};

Z7_COM7F_IMF(CSourceInStream::Read(void *data, UInt32 size, UInt32 *processedSize))
{
    if (processedSize)
        *processedSize = 0;
    const UInt64 remaining = Source->size - Pos;
    if (size > remaining)
        size = (UInt32)remaining;
    if (size == 0)
        return S_OK;
    UInt64 got = 0;
    if (Source->read)
    {
        const int64_t n = Source->read(Source->user_data, data, size);
        if (n < 0 || (UInt64)n > size)
        {
            SetLastError("Source read failed");
            return E_FAIL;
        }
        got = (UInt64)n;
        if (got == 0)
        {
            SetLastError("Source ended before its declared size");
            return E_FAIL;
        }
    }
    else
    {
        if (Source->read_at(Source->user_data, Pos, data, size) != 0)
        {
            SetLastError("Source read_at failed");
            return E_FAIL;
        }
        got = size;
    }
    Pos += got;
    if (processedSize)
        *processedSize = (UInt32)got;
    return S_OK;
}

static FILETIME SourceMTime(const SevenZipSource &source)
{
    UInt64 ft = source.mtime_filetime;
    if (ft == 0)
        ft = ((UInt64)std::time(nullptr) + 11644473600ULL) * 10000000ULL;
    FILETIME result;
    result.dwLowDateTime = (DWORD)ft;
    result.dwHighDateTime = (DWORD)(ft >> 32);
    return result;
}
#endif

class CArchiveUpdateCallback Z7_final:
    public IArchiveUpdateCallback2,
    public ICryptoGetTextPassword2,
    public CMyUnknownImp
{
    Z7_IFACES_IMP_UNK_2(IArchiveUpdateCallback2, ICryptoGetTextPassword2)
    Z7_IFACE_COM7_IMP(IProgress)
    Z7_IFACE_COM7_IMP(IArchiveUpdateCallback)
public:
    const std::vector<CDirItem> *DirItems;
    UInt64 Total;
    SevenZipProgressCb OnProgress;
    void *UserData;
    bool Cancelled;

    CArchiveUpdateCallback(): DirItems(nullptr), Total(0), OnProgress(nullptr),
        UserData(nullptr), Cancelled(false) {}
};

Z7_COM7F_IMF(CArchiveUpdateCallback::SetTotal(UInt64 size))
{
    Total = size;
    return S_OK;
}

Z7_COM7F_IMF(CArchiveUpdateCallback::SetCompleted(const UInt64 *completeValue))
{
    if (OnProgress)
    {
        float percent = -1.0f;
        if (completeValue && Total > 0)
            percent = (float)((double)*completeValue * 100.0 / (double)Total);
        if (!OnProgress("Compressing", percent, UserData))
        {
            Cancelled = true;
            return E_ABORT;
        }
    }
    return S_OK;
}

Z7_COM7F_IMF(CArchiveUpdateCallback::GetUpdateItemInfo(UInt32, Int32 *newData,
    Int32 *newProperties, UInt32 *indexInArchive))
{
    if (newData) *newData = BoolToInt(true);
    if (newProperties) *newProperties = BoolToInt(true);
    if (indexInArchive) *indexInArchive = (UInt32)(Int32)-1;
    return S_OK;
}

Z7_COM7F_IMF(CArchiveUpdateCallback::GetProperty(UInt32 index, PROPID propID, PROPVARIANT *value))
{
    NCOM::CPropVariant prop;
    if (propID == kpidIsAnti)
    {
        prop = false;
    }
    else
    {
        const CDirItem &di = (*DirItems)[index];
#ifdef SEVENZIP_WITH_STREAMING
        if (di.Source)
        {
            switch (propID)
            {
                case kpidPath: prop = di.PathForHandler; break;
                case kpidIsDir: prop = false; break;
                case kpidSize: prop = (UInt64)di.Source->size; break;
                case kpidAttrib: prop = (UInt32)FILE_ATTRIBUTE_ARCHIVE; break;
                case kpidMTime: prop = SourceMTime(*di.Source); break;
                default: break;
            }
            prop.Detach(value);
            return S_OK;
        }
#endif
        switch (propID)
        {
            case kpidPath: prop = di.PathForHandler; break;
            case kpidIsDir: prop = di.Fi.IsDir(); break;
            case kpidSize: prop = di.Fi.Size; break;
            case kpidAttrib: prop = (UInt32)di.Fi.GetWinAttrib(); break;
            case kpidCTime: PropVariant_SetFrom_FiTime(prop, di.Fi.CTime); break;
            case kpidATime: PropVariant_SetFrom_FiTime(prop, di.Fi.ATime); break;
            case kpidMTime: PropVariant_SetFrom_FiTime(prop, di.Fi.MTime); break;
            default: break;
        }
    }
    prop.Detach(value);
    return S_OK;
}

Z7_COM7F_IMF(CArchiveUpdateCallback::GetStream(UInt32 index, ISequentialInStream **inStream))
{
    const CDirItem &di = (*DirItems)[index];
#ifdef SEVENZIP_WITH_STREAMING
    if (di.Source)
    {
        CSourceInStream *spec = new CSourceInStream;
        CMyComPtr<ISequentialInStream> loc(spec);
        spec->Source = di.Source;
        *inStream = loc.Detach();
        return S_OK;
    }
#endif
    if (di.Fi.IsDir())
        return S_OK;

    CInFileStream *inStreamSpec = new CInFileStream;
    CMyComPtr<ISequentialInStream> inStreamLoc(inStreamSpec);
    if (!inStreamSpec->Open(di.FullPath))
    {
        SetLastError("Cannot open input file for reading");
        return E_ABORT;
    }
    *inStream = inStreamLoc.Detach();
    return S_OK;
}

Z7_COM7F_IMF(CArchiveUpdateCallback::SetOperationResult(Int32))
{
    return S_OK;
}

Z7_COM7F_IMF(CArchiveUpdateCallback::GetVolumeSize(UInt32, UInt64 *))
{
    return S_FALSE;
}

Z7_COM7F_IMF(CArchiveUpdateCallback::GetVolumeStream(UInt32, ISequentialOutStream **))
{
    return E_NOTIMPL;
}

Z7_COM7F_IMF(CArchiveUpdateCallback::CryptoGetTextPassword2(Int32 *passwordIsDefined, BSTR *password))
{
    *passwordIsDefined = BoolToInt(false);
    return StringToBstr(UString(), password);
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

// Mirrors RegisterArc.h's own CreateArc()/CreateArcOut() factory pattern exactly
// ("static IInArchive *CreateArc() { return new c; }") -- a fresh CHandler upcasts
// directly and unambiguously to whichever single interface pointer type is asked for
// here; there is no IUnknown-mediated QueryInterface step, and no need for one.
static IInArchive *NewInHandler(SevenZipFormat format)
{
    switch (format)
    {
        case SEVENZIP_FORMAT_7Z: return new NArchive::N7z::CHandler();
        case SEVENZIP_FORMAT_ZIP: return new NArchive::NZip::CHandler();
        default: return nullptr;
    }
}

static IOutArchive *NewOutHandler(SevenZipFormat format)
{
    switch (format)
    {
        case SEVENZIP_FORMAT_7Z: return new NArchive::N7z::CHandler();
        case SEVENZIP_FORMAT_ZIP: return new NArchive::NZip::CHandler();
        default: return nullptr;
    }
}

SevenZipArchive *sevenzip_open(const char *path, SevenZipFormat format,
                                 SevenZipProgressCb on_progress, void *user_data)
{
    CMyComPtr<IInArchive> archive = NewInHandler(format);
    if (!archive)
    {
        SetLastError("Unsupported format");
        return nullptr;
    }

    CInFileStream *fileSpec = new CInFileStream;
    CMyComPtr<IInStream> file = fileSpec;
    if (!fileSpec->Open(us2fs(Utf8ToUString(path))))
    {
        SetLastError("Cannot open archive file");
        return nullptr;
    }

    CArchiveOpenCallback *openCallbackSpec = new CArchiveOpenCallback(on_progress, user_data);
    CMyComPtr<IArchiveOpenCallback> openCallback(openCallbackSpec);

    const UInt64 scanSize = 1 << 23;
    if (archive->Open(file, &scanSize, openCallback) != S_OK)
    {
        SetLastError("Cannot open file as archive (unrecognized format, corrupt, or encrypted)");
        return nullptr;
    }

    SevenZipArchive *result = new SevenZipArchive();
    result->Archive = archive;

    UInt32 numItems = 0;
    archive->GetNumberOfItems(&numItems);
    result->Entries.reserve(numItems);
    for (UInt32 i = 0; i < numItems; i++)
    {
        CCachedEntry entry;

        NCOM::CPropVariant pathProp;
        archive->GetProperty(i, kpidPath, &pathProp);
        UString path;
        if (pathProp.vt == VT_BSTR)
            path = pathProp.bstrVal;
        entry.Path = UStringToUtf8(path);
        // Normalize to '/' separators as documented in sevenzip_lib.h.
        for (size_t c = 0; c < entry.Path.size(); c++)
            if (entry.Path[c] == '\\')
                entry.Path[c] = '/';

        entry.Size = GetUInt64Prop(archive, i, kpidSize);
        entry.PackSize = GetUInt64Prop(archive, i, kpidPackSize);
        entry.IsDir = GetBoolProp(archive, i, kpidIsDir);

        NCOM::CPropVariant attribProp;
        archive->GetProperty(i, kpidAttrib, &attribProp);
        entry.Attrib = (attribProp.vt == VT_UI4) ? attribProp.ulVal : 0;

        NCOM::CPropVariant mtimeProp;
        archive->GetProperty(i, kpidMTime, &mtimeProp);
        FileTimeFromProp(mtimeProp, entry.MTime);

        result->Entries.push_back(entry);
    }

    return result;
}

void sevenzip_close(SevenZipArchive *archive)
{
    if (!archive)
        return;
    if (archive->Archive)
        archive->Archive->Close();
    delete archive;
}

int sevenzip_get_entry_count(SevenZipArchive *archive)
{
    if (!archive)
        return 0;
    return (int)archive->Entries.size();
}

int sevenzip_get_entry(SevenZipArchive *archive, int index, SevenZipEntry *out_entry)
{
    if (!archive || !out_entry || index < 0 || (size_t)index >= archive->Entries.size())
    {
        SetLastError("Invalid archive handle or index");
        return -1;
    }
    const CCachedEntry &e = archive->Entries[(size_t)index];
    out_entry->path = e.Path.c_str();
    out_entry->size = e.Size;
    out_entry->packed_size = e.PackSize;
    out_entry->attributes = e.Attrib;
    out_entry->mtime = e.MTime;
    out_entry->is_dir = e.IsDir ? 1 : 0;
    return 0;
}

static int ExtractOne(SevenZipArchive *archive, int index, bool toBuffer,
                       const char *outPath, uint8_t **outData, size_t *outLen,
                       SevenZipProgressCb on_progress, void *user_data
#ifdef SEVENZIP_WITH_STREAMING
                       , SevenZipDataCb on_data = nullptr, bool streamOnly = false
#endif
                       )
{
    if (!archive || index < 0 || (size_t)index >= archive->Entries.size())
    {
        SetLastError("Invalid archive handle or index");
        return -1;
    }

    CArchiveExtractCallback *extractCallbackSpec = new CArchiveExtractCallback();
    CMyComPtr<IArchiveExtractCallback> extractCallback(extractCallbackSpec);
    extractCallbackSpec->ToBuffer = toBuffer;
    if (!toBuffer && outPath)
        extractCallbackSpec->OutFilePath = us2fs(Utf8ToUString(outPath));
    extractCallbackSpec->OnProgress = on_progress;
    extractCallbackSpec->UserData = user_data;
#ifdef SEVENZIP_WITH_STREAMING
    extractCallbackSpec->OnData = on_data;
    extractCallbackSpec->DataUserData = user_data;
    extractCallbackSpec->StreamOnly = streamOnly;
#endif

    const UInt32 indices[1] = { (UInt32)index };
    HRESULT result = archive->Archive->Extract(indices, 1, false, extractCallback);

    if (extractCallbackSpec->Cancelled)
    {
        SetLastError("Cancelled");
        return -6;
    }
#ifdef SEVENZIP_WITH_STREAMING
    if (extractCallbackSpec->DataStopped)
    {
        SetLastError("Stopped by data callback");
        return -6;
    }
#endif
    if (result != S_OK)
    {
        SetLastErrorHR("Extract failed", result);
        return -1;
    }
    if (extractCallbackSpec->Failed)
        return -1; // SetOperationResult already set the message

    if (toBuffer)
    {
        CMemOutStream *mem = extractCallbackSpec->MemStreamSpec;
        if (!mem)
        {
            SetLastError("No data extracted");
            return -1;
        }
        uint8_t *buf = (uint8_t *)malloc(mem->Buf.size() ? mem->Buf.size() : 1);
        if (!buf)
        {
            SetLastError("Out of memory");
            return -1;
        }
        if (!mem->Buf.empty())
            memcpy(buf, mem->Buf.data(), mem->Buf.size());
        *outData = buf;
        *outLen = mem->Buf.size();
    }

    return 0;
}

int sevenzip_extract_entry_to_file(SevenZipArchive *archive, int index, const char *out_path,
                                     SevenZipProgressCb on_progress, void *user_data)
{
    return ExtractOne(archive, index, false, out_path, nullptr, nullptr, on_progress, user_data);
}

int sevenzip_extract_entry_to_buffer(SevenZipArchive *archive, int index,
                                       uint8_t **out_data, size_t *out_len,
                                       SevenZipProgressCb on_progress, void *user_data)
{
    return ExtractOne(archive, index, true, nullptr, out_data, out_len, on_progress, user_data);
}

#ifdef SEVENZIP_WITH_STREAMING
int sevenzip_extract_entry_stream(SevenZipArchive *archive, int index, const char *out_path,
                                    SevenZipDataCb on_data, SevenZipProgressCb on_progress,
                                    void *user_data)
{
    // out_path == NULL means "stream only": no file is created, so the decompressed bytes
    // reach on_data and are then dropped.
    const bool streamOnly = (out_path == nullptr);
    return ExtractOne(archive, index, false, out_path, nullptr, nullptr, on_progress, user_data,
                      on_data, streamOnly);
}
#endif

void sevenzip_free_buffer(uint8_t *data)
{
    free(data);
}

static int CreateArchiveImpl(const char *out_path, const SevenZipCreateOptions *options,
                             const std::vector<CDirItem> &dirItems);

int sevenzip_create_archive(const char *out_path, const SevenZipCreateOptions *options)
{
    if (!options || options->count <= 0 || !options->input_paths || !options->archive_names)
    {
        SetLastError("Invalid create options");
        return -1;
    }
    if (options->format == SEVENZIP_FORMAT_ZIP && options->method == SEVENZIP_METHOD_LZMA2)
    {
        SetLastError("LZMA2 is only valid for the 7z format, not ZIP");
        return -1;
    }

    std::vector<CDirItem> dirItems;
    dirItems.reserve((size_t)options->count);
    for (int i = 0; i < options->count; i++)
    {
        CDirItem di;
        FString fullPath = us2fs(Utf8ToUString(options->input_paths[i]));
        if (!di.Fi.Find(fullPath))
        {
            SetLastError("Cannot find input file");
            return -1;
        }
        di.FullPath = fullPath;
        di.PathForHandler = Utf8ToUString(options->archive_names[i]);
        dirItems.push_back(di);
    }
    return CreateArchiveImpl(out_path, options, dirItems);
}

static int CreateArchiveImpl(const char *out_path, const SevenZipCreateOptions *options,
                             const std::vector<CDirItem> &dirItems)
{
    COutFileStream *outFileStreamSpec = new COutFileStream;
    CMyComPtr<IOutStream> outFileStream = outFileStreamSpec;
    if (!outFileStreamSpec->Create_ALWAYS(us2fs(Utf8ToUString(out_path))))
    {
        SetLastError("Cannot create output archive file");
        return -1;
    }

    CMyComPtr<IOutArchive> outArchive = NewOutHandler(options->format);
    if (!outArchive)
    {
        SetLastError("Unsupported format");
        return -1;
    }

    {
        CMyComPtr<ISetProperties> setProperties;
        outArchive->QueryInterface(IID_ISetProperties, (void **)&setProperties);
        if (!setProperties)
        {
            SetLastError("ISetProperties unsupported by this format handler");
            return -1;
        }

        const wchar_t *methodName = MethodName(options->method);
        if (options->format == SEVENZIP_FORMAT_7Z)
        {
            // "s" (solid) is always forced off -- see sevenzip_create_archive's own
            // doc comment: every file must stay independently extractable.
            const wchar_t *names[3] = { L"m", L"s", L"x" };
            NCOM::CPropVariant values[3] = { methodName, false, (UInt32)options->level };
            if (setProperties->SetProperties(names, values, 3) != S_OK)
            {
                SetLastError("SetProperties failed (invalid method/level for 7z?)");
                return -1;
            }
        }
        else
        {
            // Plain ZIP has no solid concept at all, so only method + level apply.
            const wchar_t *names[2] = { L"m", L"x" };
            NCOM::CPropVariant values[2] = { methodName, (UInt32)options->level };
            if (setProperties->SetProperties(names, values, 2) != S_OK)
            {
                SetLastError("SetProperties failed (invalid method/level for zip?)");
                return -1;
            }
        }
    }

    CArchiveUpdateCallback *updateCallbackSpec = new CArchiveUpdateCallback();
    CMyComPtr<IArchiveUpdateCallback2> updateCallback(updateCallbackSpec);
    updateCallbackSpec->DirItems = &dirItems;
    updateCallbackSpec->OnProgress = options->on_progress;
    updateCallbackSpec->UserData = options->user_data;

    HRESULT result = outArchive->UpdateItems(outFileStream, (UInt32)dirItems.size(), updateCallback);

    if (updateCallbackSpec->Cancelled)
    {
        SetLastError("Cancelled");
        return -6;
    }
    if (result != S_OK)
    {
        SetLastErrorHR("Archive creation failed", result);
        return -1;
    }

    return 0;
}

#ifdef SEVENZIP_WITH_STREAMING
int sevenzip_create_archive_from_sources(const char *out_path, const SevenZipCreateOptions *options,
                                         const SevenZipSource *sources, int count)
{
    if (!options || !sources || count <= 0 || !out_path)
    {
        SetLastError("Invalid create options");
        return -1;
    }
    if (options->format == SEVENZIP_FORMAT_ZIP && options->method == SEVENZIP_METHOD_LZMA2)
    {
        SetLastError("LZMA2 is only valid for the 7z format, not ZIP");
        return -1;
    }
    std::vector<CDirItem> dirItems((size_t)count);
    for (int i = 0; i < count; i++)
    {
        const SevenZipSource &src = sources[i];
        if (!src.archive_name || (!src.read && !src.read_at))
        {
            SetLastError("Source needs archive_name and read or read_at");
            return -1;
        }
        dirItems[(size_t)i].PathForHandler = Utf8ToUString(src.archive_name);
        dirItems[(size_t)i].Source = &src;
    }
    return CreateArchiveImpl(out_path, options, dirItems);
}

// ---------------------------------------------------------------------------
// Entry reader: sevenzip_extract_entry_stream on a worker thread, feeding a bounded pipe.
// ---------------------------------------------------------------------------

struct SevenZipEntryReader
{
    SevenZipArchive *archive = nullptr;
    int index = 0;
    uint64_t size = 0;
    uint64_t pos = 0;  // next byte the consumer receives

    std::thread worker;
    std::mutex m;
    std::condition_variable cv;
    std::deque<std::vector<uint8_t>> chunks;
    size_t front_offset = 0;  // consumed bytes of chunks.front()
    uint64_t buffered = 0;
    bool running = false, done = false, failed = false, abort = false;
    std::string error;

    static constexpr uint64_t kCapacity = 8u << 20;

    static int OnData(uint32_t, uint64_t, const void *data, uint32_t n, void *user)
    {
        auto *r = static_cast<SevenZipEntryReader *>(user);
        std::unique_lock<std::mutex> lock(r->m);
        r->cv.wait(lock, [r] { return r->abort || r->buffered < kCapacity; });
        if (r->abort)
            return 0;
        const uint8_t *p = static_cast<const uint8_t *>(data);
        r->chunks.emplace_back(p, p + n);
        r->buffered += n;
        r->cv.notify_all();
        return 1;
    }

    void Start()
    {
        chunks.clear();
        front_offset = 0;
        buffered = 0;
        pos = 0;
        done = failed = abort = false;
        running = true;
        worker = std::thread([this] {
            const int rc = sevenzip_extract_entry_stream(archive, index, nullptr, OnData, nullptr, this);
            std::string message = rc ? sevenzip_get_last_error() : "";
            std::lock_guard<std::mutex> lock(m);
            done = true;
            failed = rc != 0 && !abort;
            error = message;
            cv.notify_all();
        });
    }

    void Stop()
    {
        if (!running)
            return;
        {
            std::lock_guard<std::mutex> lock(m);
            abort = true;
            cv.notify_all();
        }
        worker.join();
        running = false;
    }

    // Copies up to n bytes; 0 at the end of the entry, -1 on a decode error.
    int64_t Pull(uint8_t *out, uint64_t n)
    {
        if (!running)
            Start();
        std::unique_lock<std::mutex> lock(m);
        cv.wait(lock, [this] { return buffered > 0 || done; });
        if (buffered == 0)
        {
            if (failed)
            {
                SetLastError(error.empty() ? "Decoding failed" : error.c_str());
                return -1;
            }
            return 0;
        }
        uint64_t copied = 0;
        while (copied < n && !chunks.empty())
        {
            std::vector<uint8_t> &front = chunks.front();
            const size_t take = (size_t)std::min<uint64_t>(n - copied, front.size() - front_offset);
            memcpy(out + copied, front.data() + front_offset, take);
            copied += take;
            front_offset += take;
            if (front_offset == front.size())
            {
                chunks.pop_front();
                front_offset = 0;
            }
        }
        buffered -= copied;
        pos += copied;
        cv.notify_all();
        return (int64_t)copied;
    }
};

SevenZipEntryReader *sevenzip_entry_reader_open(const char *archive_path, SevenZipFormat format, int index)
{
    SevenZipArchive *archive = sevenzip_open(archive_path, format, nullptr, nullptr);
    if (!archive)
        return nullptr;
    SevenZipEntry entry;
    memset(&entry, 0, sizeof entry);
    if (sevenzip_get_entry(archive, index, &entry) != 0 || entry.is_dir)
    {
        if (entry.is_dir)
            SetLastError("Entry is a directory");
        sevenzip_close(archive);
        return nullptr;
    }
    auto *r = new SevenZipEntryReader;
    r->archive = archive;
    r->index = index;
    r->size = entry.size;
    return r;
}

uint64_t sevenzip_entry_reader_size(SevenZipEntryReader *reader)
{
    return reader ? reader->size : 0;
}

int64_t sevenzip_entry_reader_read(SevenZipEntryReader *reader, void *buf, uint64_t size)
{
    if (!reader || (!buf && size))
    {
        SetLastError("NULL argument");
        return -1;
    }
    return size ? reader->Pull(static_cast<uint8_t *>(buf), size) : 0;
}

int sevenzip_entry_reader_read_at(SevenZipEntryReader *reader, uint64_t offset, void *buf, uint64_t size)
{
    if (!reader || (!buf && size))
    {
        SetLastError("NULL argument");
        return -1;
    }
    if (offset > reader->size || size > reader->size - offset)
    {
        SetLastError("read past end of entry");
        return -1;
    }
    if (offset < reader->pos)  // backward: restart decoding from the entry's start
    {
        reader->Stop();
        reader->pos = 0;
    }
    uint8_t scratch[65536];
    while (reader->pos < offset)  // forward: decode and discard
    {
        const int64_t n = reader->Pull(scratch, std::min<uint64_t>(sizeof scratch, offset - reader->pos));
        if (n <= 0)
        {
            if (n == 0)
                SetLastError("Entry ended early");
            return -1;
        }
    }
    uint8_t *out = static_cast<uint8_t *>(buf);
    uint64_t got = 0;
    while (got < size)
    {
        const int64_t n = reader->Pull(out + got, size - got);
        if (n <= 0)
        {
            if (n == 0)
                SetLastError("Entry ended early");
            return -1;
        }
        got += (uint64_t)n;
    }
    return 0;
}

void sevenzip_entry_reader_close(SevenZipEntryReader *reader)
{
    if (!reader)
        return;
    reader->Stop();
    sevenzip_close(reader->archive);
    delete reader;
}
#endif
