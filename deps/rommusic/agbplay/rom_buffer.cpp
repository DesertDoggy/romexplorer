// agbplay's Rom, for ROM bytes handed over in memory. Upstream's Rom.cpp also opens files, zips
// and GSF libraries (libzip, zlib), none of which this library needs: the app reads the ROM.
#include "Rom.hpp"

std::unique_ptr<Rom> Rom::globalInstance;

Rom Rom::LoadFromBufferCopy(std::span<uint8_t> buffer)
{
    Rom rom;
    rom.romContainer.assign(buffer.begin(), buffer.end());
    rom.romData = rom.romContainer;
    rom.Verify();
    return rom;
}

Rom Rom::LoadFromBufferRef(std::span<uint8_t> buffer)
{
    Rom rom;
    rom.romData = buffer;
    rom.Verify();
    return rom;
}

std::string Rom::ReadString(size_t pos, size_t limit) const
{
    std::string result;
    for (size_t i = 0; i < limit; i++) {
        char c = static_cast<char>(ReadU8(pos + i));
        if (c == '\0')
            break;
        result += c;
    }
    return result;
}

std::string Rom::GetROMCode() const
{
    return ReadString(0xAC, 4);
}

bool Rom::IsGsf() const
{
    return isGsf;
}

// Only the size is checked: upstream also requires the boot logo and header checksum, which
// translations and other patched ROMs often break while their music is untouched.
void Rom::Verify()
{
    if (romData.size() > AGB_ROM_SIZE || romData.size() < 0x200)
        throw Xcept("Illegal ROM size");
}
