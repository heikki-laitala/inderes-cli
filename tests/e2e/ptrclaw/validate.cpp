// E2E validator for ptrclaw integration.
//
// Links directly against ptrclaw's own `skill.cpp` + `util.cpp` and calls
// `ptrclaw::parse_skill_file()` on the SKILL.md produced by
// `inderes install-skill ptrclaw`. Also invokes `inderes --version` and
// `inderes whoami` to prove the CLI is callable (no auth needed).
//
// Build:
//   g++ -std=c++17 -O1 -Wall -Werror \
//       -I<ptrclaw>/src \
//       validate.cpp <ptrclaw>/src/skill.cpp <ptrclaw>/src/util.cpp \
//       -o validate
//
// Env:
//   INDERES_BIN  — path to the inderes binary (default: "inderes" on PATH).

#include "skill.hpp"

#include <array>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <sstream>
#include <string>

namespace {

[[noreturn]] void fatal(const std::string& msg) {
    std::cerr << "FAIL  " << msg << '\n';
    std::exit(1);
}

std::string inderes_bin() {
    const char* e = std::getenv("INDERES_BIN");
    return (e && *e) ? std::string(e) : std::string("inderes");
}

int run(const std::string& cmd, std::string* stdout_capture = nullptr) {
    std::array<char, 4096> buf{};
    std::string out;
    FILE* p = ::popen(cmd.c_str(), "r");
    if (!p) return -1;
    while (std::fgets(buf.data(), buf.size(), p) != nullptr) {
        out.append(buf.data());
    }
    int status = ::pclose(p);
    if (stdout_capture) *stdout_capture = out;
    return status;
}

std::string read_file(const std::filesystem::path& p) {
    std::ifstream in(p);
    if (!in) fatal("could not read file: " + p.string());
    std::ostringstream ss;
    ss << in.rdbuf();
    return ss.str();
}

} // namespace

int main() {
    namespace fs = std::filesystem;

    // ---------------------------------------------------------------- install
    fs::path tmp = fs::temp_directory_path() / "inderes-e2e-ptrclaw";
    fs::remove_all(tmp);
    fs::create_directories(tmp);
    fs::path skill_path = tmp / "inderes" / "SKILL.md";
    fs::create_directories(skill_path.parent_path());

    std::string install_cmd = inderes_bin() + " install-skill ptrclaw --dest " +
                              skill_path.string() + " --force";
    if (run(install_cmd) != 0) {
        fatal("inderes install-skill ptrclaw failed");
    }
    if (!fs::exists(skill_path)) {
        fatal("skill file missing: " + skill_path.string());
    }

    // ---------------------------------------------------------- parse via ptrclaw
    const std::string content = read_file(skill_path);
    auto parsed = ptrclaw::parse_skill_file(content, skill_path.string());
    if (!parsed) {
        fatal("ptrclaw::parse_skill_file returned nullopt");
    }
    if (parsed->name != "inderes") {
        fatal("expected name=\"inderes\", got \"" + parsed->name + "\"");
    }
    if (parsed->description.size() < 20) {
        fatal("description missing or too short (got " +
              std::to_string(parsed->description.size()) + " chars)");
    }
    if (parsed->prompt.empty()) {
        fatal("skill body (prompt) is empty");
    }

    // --------------------------------------------------------- cli executable
    std::string version_out;
    if (run(inderes_bin() + " --version", &version_out) != 0) {
        fatal("inderes --version exited non-zero: " + version_out);
    }
    if (version_out.rfind("inderes ", 0) != 0) {
        fatal("unexpected version output: " + version_out);
    }

    std::string whoami_out;
    if (run(inderes_bin() + " whoami", &whoami_out) != 0) {
        fatal("inderes whoami exited non-zero: " + whoami_out);
    }
    if (whoami_out.find("Not signed in") == std::string::npos) {
        fatal("unexpected whoami output: " + whoami_out);
    }

    // ---------------------------------------------------------------- done
    std::cout << "OK  ptrclaw skill loader integration\n";
    std::cout << "    skill.name        = " << parsed->name << '\n';
    std::cout << "    skill.description = " << parsed->description.size() << " chars\n";
    std::cout << "    inderes --version = "
              << version_out.substr(0, version_out.find_last_not_of(" \t\r\n") + 1) << '\n';
    std::cout << "    inderes whoami    = "
              << whoami_out.substr(0, whoami_out.find_last_not_of(" \t\r\n") + 1) << '\n';

    fs::remove_all(tmp);
    return 0;
}
