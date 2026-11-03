// cemuwua stand-in for the one boost string algorithm Cemu's FileStream uses.
#pragma once
#include <string_view>
#include <cctype>
namespace boost {
inline bool iequals(std::string_view a, std::string_view b)
{
	if (a.size() != b.size())
		return false;
	for (size_t i = 0; i < a.size(); i++)
		if (std::tolower((unsigned char)a[i]) != std::tolower((unsigned char)b[i]))
			return false;
	return true;
}
}
