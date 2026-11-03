// cemuwua stand-in for boost::nowide::widen / narrow (UTF-8 <-> UTF-16), used by Cemu's
// Windows FileStream.
#pragma once
#include <string>
#include <windows.h>
namespace boost { namespace nowide {
inline std::wstring widen(const char* s, size_t n)
{
	if (n == 0)
		return {};
	int len = MultiByteToWideChar(CP_UTF8, 0, s, (int)n, nullptr, 0);
	std::wstring out(len, L'\0');
	MultiByteToWideChar(CP_UTF8, 0, s, (int)n, out.data(), len);
	return out;
}
inline std::wstring widen(const std::string& s) { return widen(s.data(), s.size()); }
inline std::string narrow(const wchar_t* s, size_t n)
{
	if (n == 0)
		return {};
	int len = WideCharToMultiByte(CP_UTF8, 0, s, (int)n, nullptr, 0, nullptr, nullptr);
	std::string out(len, '\0');
	WideCharToMultiByte(CP_UTF8, 0, s, (int)n, out.data(), len, nullptr, nullptr);
	return out;
}
inline std::string narrow(const std::wstring& s) { return narrow(s.data(), s.size()); }
inline std::string narrow(const wchar_t* s) { return narrow(s, wcslen(s)); }
} }
