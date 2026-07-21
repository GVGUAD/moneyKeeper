-- Validation permits ordinary reads and writes while it scans legacy rows.
ALTER TABLE email_connections
    VALIDATE CONSTRAINT email_connections_normalized_address_check;
