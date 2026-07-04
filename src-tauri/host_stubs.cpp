// Host-only link stubs for sherpa-onnx's piper-phonemize TTS support.
//
// The static sherpa-onnx archive references espeak symbols from its own
// (unused-by-us) TTS path; on Android the prebuilt bundles espeak-ng, but the
// host prebuilt does not, which left `cargo test` / host bins unable to link
// the library at all. Verba never touches sherpa's TTS (speech synthesis is
// piper.rs + ort, espeak-free), so these can never be called — they exist
// purely to satisfy the linker, aborting loudly if that assumption ever
// breaks. Compiled by build.rs for non-Android targets only.

#include <cstdlib>
#include <string>
#include <vector>

extern "C" int espeak_Initialize(int, int, const char *, int) {
    std::abort();
}

namespace piper {

struct eSpeakPhonemeConfig;

void phonemize_eSpeak(std::string, eSpeakPhonemeConfig &,
                      std::vector<std::vector<char32_t>> &) {
    std::abort();
}

} // namespace piper
