package com.sqlguard.replay.report;

import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.SerializationFeature;
import com.sqlguard.replay.plan.PlanFinding;
import com.sqlguard.replay.replay.ReplayResult;
import com.sqlguard.replay.replay.StatementTimings;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;

/**
 * 把 ReplayResult 聚合成 ReplayReport，写出 replay-report.json + replay-report.html。
 */
public final class ReportWriter {

    private static final ObjectMapper MAPPER = new ObjectMapper()
            .enable(SerializationFeature.INDENT_OUTPUT);

    private ReportWriter() {
    }

    /**
     * 由结果列表构建报告（含 summary 聚合）。
     */
    public static ReplayReport buildReport(List<ReplayResult> results) {
        int total = results.size();
        int replayed = 0;
        int skipped = 0;
        int slowWarn = 0;
        int slowError = 0;
        int errors = 0;
        int planFindings = 0;

        List<StatementReport> stmts = new ArrayList<StatementReport>(results.size());
        for (ReplayResult r : results) {
            stmts.add(toStatementReport(r));
            if (r.isSkipped()) {
                skipped++;
            } else {
                replayed++;
            }
            if (r.getError() != null) {
                errors++;
            }
            if (r.getSlowLevel() == ReplayResult.SlowLevel.warn) {
                slowWarn++;
            } else if (r.getSlowLevel() == ReplayResult.SlowLevel.error) {
                slowError++;
            }
            planFindings += r.getFindings().size();
        }

        ReplayReport report = new ReplayReport();
        report.setSummary(new ReplayReport.Summary(
                total, replayed, skipped, slowWarn, slowError, errors, planFindings));
        report.setStatements(stmts);
        return report;
    }

    private static StatementReport toStatementReport(ReplayResult r) {
        StatementReport sr = new StatementReport();
        sr.setId(r.getId());
        sr.setType(r.getType());
        sr.setSource(r.getSource());
        sr.setLine(r.getLine());
        sr.setSql(r.getSql());

        if (r.getTimings() != null) {
            StatementTimings t = r.getTimings();
            sr.setTimings(new StatementReport.Timings(
                    t.getIterations(), t.getMinMs(), t.getP50Ms(), t.getP99Ms(), t.getAvgMs()));
        }
        sr.setSlowLevel(r.getSlowLevel().name());
        sr.setPlanTopNode(r.getPlanTopNode());
        sr.setPlanTotalCost(r.getPlanTotalCost());
        sr.setPlanJson(r.getPlanJson());

        List<StatementReport.Finding> findings = new ArrayList<StatementReport.Finding>(r.getFindings().size());
        for (PlanFinding f : r.getFindings()) {
            findings.add(new StatementReport.Finding(
                    f.getRuleId(),
                    f.getSeverity().name(),
                    f.getMessage(),
                    f.getNodeName(),
                    f.getRelationName()));
        }
        sr.setFindings(findings);

        sr.setError(r.getError());
        sr.setSkipped(r.isSkipped());
        sr.setSkipReason(r.getSkipReason());
        return sr;
    }

    /**
     * 把报告写到输出目录（replay-report.json + replay-report.html）。
     */
    public static void write(ReplayReport report, Path outputDir) throws IOException {
        Files.createDirectories(outputDir);

        Path jsonPath = outputDir.resolve("replay-report.json");
        Files.write(jsonPath, MAPPER.writeValueAsBytes(report));

        Path htmlPath = outputDir.resolve("replay-report.html");
        Files.write(htmlPath, renderHtml(report).getBytes(StandardCharsets.UTF_8));
    }

    // ----------------------- HTML 渲染 -----------------------

    private static String renderHtml(ReplayReport report) {
        StringBuilder sb = new StringBuilder();
        sb.append("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n");
        sb.append("<meta charset=\"utf-8\">\n");
        sb.append("<title>sqlguard-replay report</title>\n");
        sb.append("<style>\n");
        sb.append("body{background:#0d1117;color:#c9d1d9;font-family:Consolas,monospace;margin:20px;}\n");
        sb.append("table{border-collapse:collapse;width:100%;font-size:13px;}\n");
        sb.append("th,td{border:1px solid #30363d;padding:6px 8px;text-align:left;vertical-align:top;}\n");
        sb.append("th{background:#161b22;position:sticky;top:0;}\n");
        sb.append("tr:nth-child(even){background:#161b22;}\n");
        sb.append(".slow-warn{color:#f0b429;font-weight:bold;}\n");
        sb.append(".slow-error{color:#f85149;font-weight:bold;}\n");
        sb.append(".slow-none{color:#58a6ff;}\n");
        sb.append(".skipped{color:#8b949e;}\n");
        sb.append(".plan-json-row td{padding:0;}\n");
        sb.append(".plan-json-row details{margin:0;}\n");
        sb.append(".plan-json-row summary{cursor:pointer;padding:4px 8px;background:#161b22;color:#8b949e;font-size:12px;}\n");
        sb.append(".plan-json-row pre{margin:0;padding:8px;max-height:400px;overflow:auto;font-size:11px;background:#0d1117;}\n");
        sb.append("pre{white-space:pre-wrap;word-break:break-all;margin:0;max-width:480px;}\n");
        sb.append(".summary{margin-bottom:16px;padding:10px;background:#161b22;border:1px solid #30363d;}\n");
        sb.append("</style>\n</head>\n<body>\n");

        ReplayReport.Summary s = report.getSummary();
        sb.append("<div class=\"summary\">");
        sb.append("<b>Replay Summary</b> &nbsp; ");
        sb.append("total=").append(s.getTotal()).append(" ");
        sb.append("replayed=").append(s.getReplayed()).append(" ");
        sb.append("skipped=").append(s.getSkipped()).append(" ");
        sb.append("slowWarn=").append(s.getSlowWarn()).append(" ");
        sb.append("slowError=").append(s.getSlowError()).append(" ");
        sb.append("errors=").append(s.getErrors()).append(" ");
        sb.append("planFindings=").append(s.getPlanFindings());
        sb.append("</div>\n");

        sb.append("<table>\n<thead><tr>");
        sb.append("<th>#</th><th>id</th><th>type</th><th>slowLevel</th>");
        sb.append("<th>p99(ms)</th><th>planTopNode</th><th>cost</th>");
        sb.append("<th>findings</th><th>sql</th><th>error/skip</th>");
        sb.append("</tr></thead>\n<tbody>\n");

        int idx = 1;
        for (StatementReport st : report.getStatements()) {
            sb.append("<tr>");
            sb.append("<td>").append(idx++).append("</td>");
            sb.append("<td>").append(esc(st.getId())).append("</td>");
            sb.append("<td>").append(esc(st.getType())).append("</td>");
            String lvl = st.getSlowLevel() == null ? "none" : st.getSlowLevel();
            sb.append("<td class=\"slow-").append(lvl).append("\">").append(esc(lvl)).append("</td>");
            sb.append("<td>").append(st.getTimings() == null ? "-" : fmt(st.getTimings().getP99Ms())).append("</td>");
            sb.append("<td>").append(esc(st.getPlanTopNode() == null ? "-" : st.getPlanTopNode())).append("</td>");
            sb.append("<td>").append(fmt(st.getPlanTotalCost())).append("</td>");
            sb.append("<td>").append(st.getFindings() == null ? 0 : st.getFindings().size()).append("</td>");
            sb.append("<td><pre>").append(esc(st.getSql())).append("</pre></td>");
            String note = "";
            if (st.isSkipped()) {
                note = "SKIP: " + (st.getSkipReason() == null ? "" : st.getSkipReason());
            } else if (st.getError() != null) {
                note = "ERR: " + st.getError();
            }
            sb.append("<td class=\"").append(st.isSkipped() ? "skipped" : "slow-error").append("\">")
                    .append(esc(note)).append("</td>");
            sb.append("</tr>\n");
            // 可折叠的原始 planJson 展示（仅在存在时渲染）
            if (st.getPlanJson() != null && !st.getPlanJson().isEmpty()) {
                sb.append("<tr class=\"plan-json-row\"><td colspan=\"10\">");
                sb.append("<details><summary>EXPLAIN JSON</summary><pre>");
                sb.append(esc(st.getPlanJson()));
                sb.append("</pre></details></td></tr>\n");
            }
        }
        sb.append("</tbody>\n</table>\n</body>\n</html>\n");
        return sb.toString();
    }

    private static String fmt(double d) {
        if (d == 0.0) {
            return "0";
        }
        return String.format("%.2f", d);
    }

    private static String esc(String s) {
        if (s == null) {
            return "";
        }
        StringBuilder sb = new StringBuilder(s.length());
        for (int i = 0; i < s.length(); i++) {
            char ch = s.charAt(i);
            switch (ch) {
                case '&':
                    sb.append("&amp;");
                    break;
                case '<':
                    sb.append("&lt;");
                    break;
                case '>':
                    sb.append("&gt;");
                    break;
                case '"':
                    sb.append("&quot;");
                    break;
                case '\'':
                    sb.append("&#39;");
                    break;
                default:
                    sb.append(ch);
            }
        }
        return sb.toString();
    }
}
