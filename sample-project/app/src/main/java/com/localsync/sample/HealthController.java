package com.localsync.sample;

import java.util.Map;

import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RestController;

/**
 * Plain liveness check for the demo/orchestration code to poll instead of
 * pulling in spring-boot-starter-actuator for one boolean.
 */
@RestController
public class HealthController {

    @GetMapping("/health")
    public Map<String, String> health() {
        return Map.of("status", "UP");
    }
}
