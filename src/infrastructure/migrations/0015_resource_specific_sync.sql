-- Target every new Banking sync job at exactly one discovered provider resource.

ALTER TABLE banking.sync_jobs
    ADD COLUMN resource_id UUID;

-- Jobs created before this migration were connection-wide. A job that
-- snapshotted only one resource is already semantically resource-specific;
-- true multi-resource legacy jobs remain NULL and continue under old rules.
WITH single_resource AS (
    SELECT sync_job_id,user_id,(array_agg(external_resource_id ORDER BY position))[1] resource_id
    FROM banking.sync_job_resources
    GROUP BY sync_job_id,user_id
    HAVING count(*) = 1
)
UPDATE banking.sync_jobs job
SET resource_id=single_resource.resource_id
FROM single_resource
WHERE job.id=single_resource.sync_job_id AND job.user_id=single_resource.user_id;

ALTER TABLE banking.sync_jobs
    ADD CONSTRAINT sync_job_resource_fk
        FOREIGN KEY (resource_id,user_id,connection_id)
        REFERENCES banking.external_resources (id,user_id,connection_id),
    ADD CONSTRAINT sync_job_target_identity
        UNIQUE (id,user_id,connection_id,resource_id);

-- Existing multi-resource rows cannot satisfy this relationship because their
-- legacy parent has no single resource_id. NOT VALID preserves those rows but
-- still enforces the invariant for every resource snapshot inserted afterward.
ALTER TABLE banking.sync_job_resources
    ADD CONSTRAINT sync_job_resource_target_fk
        FOREIGN KEY (sync_job_id,user_id,connection_id,external_resource_id)
        REFERENCES banking.sync_jobs (id,user_id,connection_id,resource_id)
        NOT VALID;

CREATE INDEX banking_jobs_by_resource
    ON banking.sync_jobs (user_id,connection_id,resource_id,created_at DESC,id)
    WHERE resource_id IS NOT NULL;
