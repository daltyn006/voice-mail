// Hardware detector: feeds the first-run tier wizard (auto-pick + visible override).
// Thresholds mirror config/models.json tiers: full needs >=12GB VRAM and >=12GB RAM,
// standard needs >=6GB VRAM and >=12GB RAM, else lite. Unknown values fail their
// condition (conservative); both unknown -> standard. The disk-space veto
// (pair bytes + headroom) lives in the GUI, which owns models.json.
#include "internal.h"
// NOTE: WIN32_LEAN_AND_MEAN comes from CMake compile definitions (see above).
#include <windows.h>
#include <dxgi1_4.h>
#include <wrl/client.h>

namespace pv {
namespace {

// Largest local-budget adapter in GB (-1 if unknown). Same choice as the VRAM guard.
// Drivers sometimes report Budget == 0; fall back to DedicatedVideoMemory then.
float adapter_vram_gb() {
    using Microsoft::WRL::ComPtr;
    ComPtr<IDXGIFactory4> f;
    if (FAILED(CreateDXGIFactory1(IID_PPV_ARGS(&f)))) return -1;
    unsigned long long bestBudget = 0, bestDedicated = 0;
    bool anyBudget = false, anyDedicated = false;
    for (UINT i = 0;; ++i) {
        ComPtr<IDXGIAdapter3> a;
        if (FAILED(f->EnumAdapters(i, reinterpret_cast<IDXGIAdapter**>(a.GetAddressOf()))))
            break;
        // NOTE: GetDesc1 (not GetDesc): only DXGI_ADAPTER_DESC1 carries
        // Flags; the v1 struct has VendorId but no Flags member.
        DXGI_ADAPTER_DESC1 desc{};
        if (FAILED(a->GetDesc1(&desc))) continue;
        // Same skip as the VRAM guard: no Basic Render Driver / software
        // adapters, or tier picks and budgets measure the wrong GPU.
        if (desc.VendorId == 0x1414 || (desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE)) continue;
        DXGI_QUERY_VIDEO_MEMORY_INFO info{};
        if (SUCCEEDED(a->QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &info)) && info.Budget) {
            anyBudget = true;
            if (info.Budget > bestBudget) bestBudget = info.Budget;
        }
        if (desc.DedicatedVideoMemory) {
            anyDedicated = true;
            if (desc.DedicatedVideoMemory > bestDedicated) bestDedicated = desc.DedicatedVideoMemory;
        }
    }
    const double GB = 1024.0 * 1024.0 * 1024.0;
    if (anyBudget) return (float)((double)bestBudget / GB);
    if (anyDedicated) return (float)((double)bestDedicated / GB);
    return -1;
}

float total_ram_gb() {
    MEMORYSTATUSEX st{sizeof(st)};
    if (!GlobalMemoryStatusEx(&st)) return -1;
    return (float)((double)st.ullTotalPhys / (1024.0 * 1024.0 * 1024.0));
}

}  // namespace

bool query_hw(HwInfo& hw, const std::string& path_for_disk) {
    hw.vram_gb = adapter_vram_gb();
    hw.ram_gb = total_ram_gb();
    hw.disk_free_bytes = 0;
    hw.disk_known = false;
    ULARGE_INTEGER freeAvail{};
    std::string anchor = path_for_disk.empty() ? "." : path_for_disk;
    if (GetDiskFreeSpaceExA(anchor.c_str(), &freeAvail, nullptr, nullptr)) {
        hw.disk_free_bytes = freeAvail.QuadPart;
        hw.disk_known = true;
    }
    return hw.vram_gb >= 0 || hw.ram_gb >= 0;
}

int pick_tier(const HwInfo& hw) {  // 0 lite, 1 standard, 2 full
    const bool vram_ok_full = hw.vram_gb >= 12.0f;
    const bool vram_ok_std = hw.vram_gb >= 6.0f;
    const bool ram_ok = hw.ram_gb >= 12.0f;
    if (hw.vram_gb < 0 && hw.ram_gb < 0) return 1;  // blind: middle
    if (vram_ok_full && (ram_ok || hw.ram_gb < 0)) return 2;
    if (vram_ok_std && (ram_ok || hw.ram_gb < 0)) return 1;
    return 0;
}

}  // namespace pv
