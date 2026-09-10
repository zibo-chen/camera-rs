#include "../cpp/camera2_metadata.h"
#include <cassert>
int main() {
    int32_t result[4];
    const int32_t sensor[] = {100, 50, 4000, 3000};
    assert(camera2_zoom_crop(sensor, 200, result));
    assert(result[0] == 1100 && result[1] == 800 && result[2] == 2000 && result[3] == 1500);
    assert(camera2_zoom_crop(sensor, 100, result) && result[0] == 100 && result[2] == 4000);
    assert(!camera2_zoom_crop(sensor, 0, result));
    const int32_t ranges[] = {15,30,30,30,30,60};
    assert(camera2_select_fps(ranges,6,30,result) && result[0] == 30 && result[1] == 30);
    assert(camera2_select_fps(ranges,2,30,result) && result[0] == 15 && result[1] == 30);
    assert(camera2_select_fps(ranges,6,120,result) && camera2_clamp_fps(120,result) == 60);
    assert(camera2_select_fps(ranges,6,10,result) && camera2_clamp_fps(10,result) == 15);
    const int32_t invalid[] = {0,30,30,15};
    assert(!camera2_select_fps(invalid,4,30,result));
    assert(!camera2_select_fps(nullptr,0,30,result));
}
