#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

void app_main(void) {
    for (;;) {
        vTaskDelay(portMAX_DELAY);
    }
}
