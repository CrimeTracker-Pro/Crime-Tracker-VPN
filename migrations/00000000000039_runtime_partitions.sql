-- The original migrations only provisioned partitions through July/August
-- 2026. Keep the current month and two future months ready on every startup
-- and maintenance pass so telemetry and access logging do not fail at a
-- month boundary.
CREATE OR REPLACE FUNCTION ensure_runtime_partitions() RETURNS void AS $$
DECLARE
    parent_name text;
    month_start date;
    month_end date;
    offset_months integer;
BEGIN
    FOREACH parent_name IN ARRAY ARRAY['bandwidth_samples', 'server_samples', 'access_logs'] LOOP
        FOR offset_months IN 0..2 LOOP
            month_start := (date_trunc('month', CURRENT_DATE)::date +
                            (offset_months || ' months')::interval)::date;
            month_end := (month_start + interval '1 month')::date;
            EXECUTE format(
                'CREATE TABLE IF NOT EXISTS %I PARTITION OF %I FOR VALUES FROM (%L) TO (%L)',
                parent_name || '_' || to_char(month_start, 'YYYY_MM'),
                parent_name,
                month_start,
                month_end
            );
        END LOOP;
    END LOOP;
END;
$$ LANGUAGE plpgsql;

SELECT ensure_runtime_partitions();
