package com.sqlguard.replay.manifest;

import com.fasterxml.jackson.databind.ObjectMapper;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;

/**
 * 加载 sql-manifest.json。
 */
public final class ManifestLoader {

    private static final ObjectMapper MAPPER = new ObjectMapper();

    private ManifestLoader() {
    }

    /**
     * 从文件加载清单。
     */
    public static Manifest load(Path manifestPath) throws IOException {
        try (java.io.InputStream in = Files.newInputStream(manifestPath)) {
            Manifest m = MAPPER.readValue(in, Manifest.class);
            if (m.getStatements() == null) {
                m.setStatements(java.util.Collections.<ManifestStatement>emptyList());
            }
            return m;
        }
    }
}
