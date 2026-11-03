// cemuwua stand-in for Cemu's config/CemuConfig.h: the region enum ncrypto.cpp names.
// Values as in Cemu's own header.
#pragma once
enum class CafeConsoleRegion
{
	JPN = 0x1,
	USA = 0x2,
	EUR = 0x4,
	AUS_DEPR = 0x8,
	CHN = 0x10,
	KOR = 0x20,
	TWN = 0x40,
	Auto = 0xFF,
};
