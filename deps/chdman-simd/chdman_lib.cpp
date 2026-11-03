#include "chdman_lib.h"

#include <cctype>
#include <cstdlib>
#include <cstring>
#include <iostream>
#include <streambuf>
#include <string>
#include <vector>

/*
 * chdman.cpp's own `main` is compiled (see Makefile.chdman_lib) with -Dmain=chdman_cli_entry,
 * which renames its entry point at the preprocessor level -- the .cpp file itself is never
 * modified. This lets chdman's real, unmodified command dispatch (command lookup in
 * s_commands[], option parsing into parameters_map, calling the matching do_* handler, and
 * catching std::error_condition/fatal_error/std::exception into a return code -- see
 * src/tools/chdman.cpp's main()) be called as an ordinary function instead of being the
 * process entry point.
 *
 * Deliberately NOT extern "C": a function literally named `main` is exempt from C++ name
 * mangling by the standard, but once renamed to chdman_cli_entry it's an ordinary function
 * and IS mangled like any other -- so this declaration must use plain C++ linkage (matching
 * how chdman.cpp's definition actually gets compiled) rather than extern "C", or the two
 * won't agree on a symbol name.
 */
int chdman_cli_entry(int argc, char* argv[]);

/*
 * chdman.cpp's main() doesn't use its own argc/argv directly -- it calls
 * `osd_get_command_line(argc, argv)` (src/osd/osdcore.cpp) first, expecting that to just
 * wrap the two into a vector<string>. On Windows, though, that function *ignores both
 * parameters* and instead calls `CommandLineToArgvW(GetCommandLineW(), &count)` -- it
 * re-fetches the *real OS process command line* rather than trusting what was passed in.
 * That's fine for a real chdman.exe process (its argc/argv and the OS command line are the
 * same thing), but fatal for chdman running as a library call inside another process: this
 * process's actual command line is the *host* program's (the Rust test binary's, here),
 * which chdman then sees as "no command given" and responds with the top-level usage
 * listing to every single call, regardless of what argv chdman_run() was actually given.
 *
 * Same fix as `main` above: compiled into chdman.cpp's call site via
 * `-Dosd_get_command_line=chdman_lib_get_command_line` (see Makefile.chdman_lib), this
 * function is what chdman.cpp actually calls instead, doing only the straightforward
 * "wrap argc/argv into a vector<string>" job the real one is supposed to do -- matching
 * osdcore.cpp's own non-Windows branch, which has no OS-command-line-refetching problem to
 * begin with. osdcore.cpp itself is untouched; other, real chdman.exe-style consumers of
 * `osd_get_command_line` elsewhere in a link are unaffected, since `-D` is scoped to this
 * one translation unit.
 */
std::vector<std::string> chdman_lib_get_command_line(int argc, char* argv[])
{
    std::vector<std::string> args;
    args.reserve(static_cast<size_t>(argc));
    for (int i = 0; i < argc; ++i)
        args.emplace_back(argv[i] ? argv[i] : "");
    return args;
}

namespace
{
// Intercepts std::cout/std::cerr writes character-by-character, splitting them into
// lines on '\r' or '\n' (chdman uses '\r' for in-place progress updates and '\n' for
// ordinary messages -- see chdman.cpp's progress()/report_error()). Each completed line
// is both appended to the full accumulated log text and, if a callback was given,
// parsed for a "<float>% complete" segment (chdman's consistent progress format) and
// delivered live.
class chdman_capture_streambuf : public std::streambuf
{
public:
    chdman_capture_streambuf(ChdmanProgressCb cb, void* user_data) : m_cb(cb), m_user_data(user_data) {}

    const std::string& text() const { return m_text; }

protected:
    int_type overflow(int_type ch) override
    {
        if (traits_type::eq_int_type(ch, traits_type::eof()))
            return traits_type::not_eof(ch);

        const char c = traits_type::to_char_type(ch);
        m_text.push_back(c);

        if (c == '\n' || c == '\r')
            flush_line();
        else
            m_line.push_back(c);

        return ch;
    }

    int sync() override
    {
        if (!m_line.empty())
            flush_line();
        return 0;
    }

private:
    void flush_line()
    {
        if (m_cb)
            m_cb(m_line.c_str(), parse_percent(m_line), m_user_data);
        m_line.clear();
    }

    // Looks for "<float>% complete" -- e.g. "Compressing, 45.3% complete... (ratio=61.2%)"
    // -- and returns the parsed float, or -1.0f if the line doesn't contain one.
    static float parse_percent(const std::string& line)
    {
        static const std::string marker = "% complete";
        const size_t marker_pos = line.find(marker);
        if (marker_pos == std::string::npos)
            return -1.0f;

        size_t start = marker_pos;
        while (start > 0 && (std::isdigit(static_cast<unsigned char>(line[start - 1])) || line[start - 1] == '.'))
            --start;
        if (start == marker_pos)
            return -1.0f;

        try
        {
            return std::stof(line.substr(start, marker_pos - start));
        }
        catch (...)
        {
            return -1.0f;
        }
    }

    ChdmanProgressCb m_cb;
    void* m_user_data;
    std::string m_text;
    std::string m_line;
};
}  // namespace

int chdman_run(int argc, const char* const* argv, char** out_log, ChdmanProgressCb on_progress, void* user_data)
{
    // chdman's main() expects argv[0] to be the program name (used only in help/usage
    // text) and argv[1] to be the command name. The public API here takes just the
    // command + options (no program name), so prepend one.
    std::vector<std::string> owned;
    owned.reserve(static_cast<size_t>(argc) + 1);
    owned.emplace_back("chdman");
    for (int i = 0; i < argc; ++i)
        owned.emplace_back(argv[i] ? argv[i] : "");

    std::vector<char*> mutable_argv;
    mutable_argv.reserve(owned.size());
    for (auto& s : owned)
        mutable_argv.push_back(s.data());

    chdman_capture_streambuf capture_buf(on_progress, user_data);
    std::ostream capture_stream(&capture_buf);
    std::streambuf* old_cout = std::cout.rdbuf(&capture_buf);
    std::streambuf* old_cerr = std::cerr.rdbuf(&capture_buf);

    int rc = 1;
    try
    {
        rc = chdman_cli_entry(static_cast<int>(mutable_argv.size()), mutable_argv.data());
    }
    catch (...)
    {
        capture_stream << "chdman_run: unhandled exception escaped chdman's command dispatch\n";
        rc = 1;
    }

    std::cout.rdbuf(old_cout);
    std::cerr.rdbuf(old_cerr);

    if (out_log)
    {
        const std::string& text = capture_buf.text();
        char* buf = static_cast<char*>(std::malloc(text.size() + 1));
        if (buf)
            std::memcpy(buf, text.c_str(), text.size() + 1);
        *out_log = buf;
    }

    return rc;
}

void chdman_free_log(char* log)
{
    std::free(log);
}
