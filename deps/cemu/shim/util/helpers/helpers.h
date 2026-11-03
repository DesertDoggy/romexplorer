// cemuwua stand-in for Cemu's util/helpers/helpers.h: the one helper ncrypto.cpp uses.
#pragma once
inline uint64 MakeU64(uint32 high, uint32 low)
{
	return ((uint64)high << 32) | ((uint64)low);
}
