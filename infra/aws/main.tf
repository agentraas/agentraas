# AgentRaaS on AWS: ECS Fargate behind a load balancer, RDS Postgres,
# ElastiCache Redis, and a KMS key that holds the master key for stored
# service credentials (CREDENTIALS_KEY_PROVIDER=aws, read with the task's
# role, so nothing secret sits in the task definition). See README.md.

terraform {
  required_version = ">= 1.5"
  required_providers {
    aws    = { source = "hashicorp/aws", version = "~> 5.60" }
    random = { source = "hashicorp/random", version = "~> 3.6" }
  }
}

provider "aws" {
  region = var.region
  default_tags { tags = { app = var.name } }
}

data "aws_availability_zones" "available" { state = "available" }

locals {
  azs        = slice(data.aws_availability_zones.available.names, 0, 2)
  https      = var.certificate_arn != ""
  public_url = var.public_url != "" ? var.public_url : "http://${aws_lb.this.dns_name}"
}

# --- Network: two public subnets (load balancer, API tasks) and two private
# ones (database, Redis). Tasks get public IPs to reach Stripe, Slack and the
# rest, which saves a NAT gateway; only the load balancer can reach them.
# ponytail: no NAT gateway; add one and move the tasks to private subnets if
# policy requires that.

resource "aws_vpc" "this" {
  cidr_block           = "10.40.0.0/16"
  enable_dns_hostnames = true
  tags                 = { Name = var.name }
}

resource "aws_internet_gateway" "this" { vpc_id = aws_vpc.this.id }

resource "aws_subnet" "public" {
  count                   = 2
  vpc_id                  = aws_vpc.this.id
  cidr_block              = cidrsubnet(aws_vpc.this.cidr_block, 8, count.index)
  availability_zone       = local.azs[count.index]
  map_public_ip_on_launch = true
  tags                    = { Name = "${var.name}-public-${count.index}" }
}

resource "aws_subnet" "private" {
  count             = 2
  vpc_id            = aws_vpc.this.id
  cidr_block        = cidrsubnet(aws_vpc.this.cidr_block, 8, count.index + 10)
  availability_zone = local.azs[count.index]
  tags              = { Name = "${var.name}-private-${count.index}" }
}

resource "aws_route_table" "public" {
  vpc_id = aws_vpc.this.id
  route {
    cidr_block = "0.0.0.0/0"
    gateway_id = aws_internet_gateway.this.id
  }
}

resource "aws_route_table_association" "public" {
  count          = 2
  subnet_id      = aws_subnet.public[count.index].id
  route_table_id = aws_route_table.public.id
}

resource "aws_security_group" "lb" {
  name   = "${var.name}-lb"
  vpc_id = aws_vpc.this.id
  dynamic "ingress" {
    for_each = local.https ? [80, 443] : [80]
    content {
      from_port   = ingress.value
      to_port     = ingress.value
      protocol    = "tcp"
      cidr_blocks = ["0.0.0.0/0"]
    }
  }
  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }
}

resource "aws_security_group" "app" {
  name   = "${var.name}-app"
  vpc_id = aws_vpc.this.id
  ingress {
    from_port       = 3000
    to_port         = 3000
    protocol        = "tcp"
    security_groups = [aws_security_group.lb.id]
  }
  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }
}

resource "aws_security_group" "data" {
  name   = "${var.name}-data"
  vpc_id = aws_vpc.this.id
  ingress {
    from_port       = 5432
    to_port         = 5432
    protocol        = "tcp"
    security_groups = [aws_security_group.app.id]
  }
  ingress {
    from_port       = 6379
    to_port         = 6379
    protocol        = "tcp"
    security_groups = [aws_security_group.app.id]
  }
}

# --- KMS: one key for the credentials master key, the database, Redis and
# the app secret.

resource "aws_kms_key" "this" {
  description             = "${var.name}: credentials master key, database and Redis at rest"
  enable_key_rotation     = true
  deletion_window_in_days = 30
}

resource "aws_kms_alias" "this" {
  name          = "alias/${var.name}"
  target_key_id = aws_kms_key.this.key_id
}

resource "random_bytes" "credentials_key" { length = 32 }

# The server accepts the key wrapped as its base64 text (kms.rs).
resource "aws_kms_ciphertext" "credentials_key" {
  count     = var.credentials_key_ciphertext == "" ? 1 : 0
  key_id    = aws_kms_key.this.key_id
  plaintext = random_bytes.credentials_key.base64
}

resource "random_password" "db" {
  length  = 32
  special = false
}

resource "random_password" "jwt" {
  length  = 64
  special = false
}

# --- Postgres and Redis, private subnets only.

resource "aws_db_subnet_group" "this" {
  name       = var.name
  subnet_ids = aws_subnet.private[*].id
}

resource "aws_db_instance" "this" {
  identifier                = var.name
  engine                    = "postgres"
  engine_version            = "16"
  instance_class            = var.db_instance_class
  allocated_storage         = var.db_allocated_storage
  storage_type              = "gp3"
  storage_encrypted         = true
  kms_key_id                = aws_kms_key.this.arn
  db_name                   = "agentraas"
  username                  = "agentraas"
  password                  = random_password.db.result
  db_subnet_group_name      = aws_db_subnet_group.this.name
  vpc_security_group_ids    = [aws_security_group.data.id]
  publicly_accessible       = false
  multi_az                  = var.db_multi_az
  backup_retention_period   = 7
  deletion_protection       = var.deletion_protection
  skip_final_snapshot       = !var.deletion_protection
  final_snapshot_identifier = "${var.name}-final"
}

resource "aws_elasticache_subnet_group" "this" {
  name       = var.name
  subnet_ids = aws_subnet.private[*].id
}

# TLS in transit (rediss://), no AUTH: reachable only from the API's
# security group inside the VPC. ponytail: add an AUTH token if your policy
# needs one.
resource "aws_elasticache_replication_group" "this" {
  replication_group_id       = var.name
  description                = "${var.name} dedup, locks and limits"
  engine                     = "redis"
  engine_version             = "7.1"
  node_type                  = var.redis_node_type
  num_cache_clusters         = 1
  port                       = 6379
  subnet_group_name          = aws_elasticache_subnet_group.this.name
  security_group_ids         = [aws_security_group.data.id]
  at_rest_encryption_enabled = true
  kms_key_id                 = aws_kms_key.this.arn
  transit_encryption_enabled = true
}

# --- App secret, read by ECS when it starts the containers.

resource "aws_secretsmanager_secret" "app" {
  name       = "${var.name}/app"
  kms_key_id = aws_kms_key.this.arn
}

resource "aws_secretsmanager_secret_version" "app" {
  secret_id = aws_secretsmanager_secret.app.id
  secret_string = jsonencode(merge(
    {
      DATABASE_URL               = "postgres://agentraas:${random_password.db.result}@${aws_db_instance.this.address}:${aws_db_instance.this.port}/agentraas?sslmode=require"
      JWT_SECRET                 = random_password.jwt.result
      CREDENTIALS_ENCRYPTION_KEY = var.credentials_key_ciphertext != "" ? var.credentials_key_ciphertext : aws_kms_ciphertext.credentials_key[0].ciphertext_blob
    },
    var.license_token != "" ? { LICENSE_TOKEN = var.license_token } : {}
  ))
}

# --- Load balancer. It appends the caller's address to X-Forwarded-For,
# hence CLIENT_IP_HEADER=x-forwarded-for:last below.

resource "aws_lb" "this" {
  name               = var.name
  load_balancer_type = "application"
  subnets            = aws_subnet.public[*].id
  security_groups    = [aws_security_group.lb.id]
}

resource "aws_lb_target_group" "api" {
  name        = var.name
  port        = 3000
  protocol    = "HTTP"
  target_type = "ip"
  vpc_id      = aws_vpc.this.id
  health_check {
    path    = "/health"
    matcher = "200"
  }
}

resource "aws_lb_listener" "http" {
  load_balancer_arn = aws_lb.this.arn
  port              = 80
  protocol          = "HTTP"
  default_action {
    type             = local.https ? "redirect" : "forward"
    target_group_arn = local.https ? null : aws_lb_target_group.api.arn
    dynamic "redirect" {
      for_each = local.https ? [1] : []
      content {
        port        = "443"
        protocol    = "HTTPS"
        status_code = "HTTP_301"
      }
    }
  }
}

resource "aws_lb_listener" "https" {
  count             = local.https ? 1 : 0
  load_balancer_arn = aws_lb.this.arn
  port              = 443
  protocol          = "HTTPS"
  ssl_policy        = "ELBSecurityPolicy-TLS13-1-2-2021-06"
  certificate_arn   = var.certificate_arn
  default_action {
    type             = "forward"
    target_group_arn = aws_lb_target_group.api.arn
  }
}

# --- ECS: a migrate container runs first (agentraas-migrate, under an
# advisory lock), then the API.

resource "aws_ecs_cluster" "this" { name = var.name }

resource "aws_cloudwatch_log_group" "api" {
  name              = "/ecs/${var.name}"
  retention_in_days = 30
}

data "aws_iam_policy_document" "ecs_assume" {
  statement {
    actions = ["sts:AssumeRole"]
    principals {
      type        = "Service"
      identifiers = ["ecs-tasks.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "execution" {
  name               = "${var.name}-execution"
  assume_role_policy = data.aws_iam_policy_document.ecs_assume.json
}

resource "aws_iam_role_policy_attachment" "execution" {
  role       = aws_iam_role.execution.name
  policy_arn = "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy"
}

resource "aws_iam_role_policy" "execution_secret" {
  role = aws_iam_role.execution.id
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      { Effect = "Allow", Action = ["secretsmanager:GetSecretValue"], Resource = [aws_secretsmanager_secret.app.arn] },
      { Effect = "Allow", Action = ["kms:Decrypt"], Resource = [aws_kms_key.this.arn] },
    ]
  })
}

# The running API: unwraps CREDENTIALS_ENCRYPTION_KEY at startup and, for
# Enterprise secret references (aws-sm://), reads only what you grant here.
resource "aws_iam_role" "task" {
  name               = "${var.name}-task"
  assume_role_policy = data.aws_iam_policy_document.ecs_assume.json
}

resource "aws_iam_role_policy" "task_kms" {
  role = aws_iam_role.task.id
  policy = jsonencode({
    Version   = "2012-10-17"
    Statement = [{ Effect = "Allow", Action = ["kms:Decrypt"], Resource = [aws_kms_key.this.arn] }]
  })
}

locals {
  secret_keys = concat(["DATABASE_URL", "JWT_SECRET", "CREDENTIALS_ENCRYPTION_KEY"], var.license_token != "" ? ["LICENSE_TOKEN"] : [])
  secrets     = [for k in local.secret_keys : { name = k, valueFrom = "${aws_secretsmanager_secret.app.arn}:${k}::" }]
  environment = [for k, v in merge({
    PORT                     = "3000"
    RUST_LOG                 = "info"
    DEPLOYMENT_MODE          = "self-hosted"
    PUBLIC_URL               = local.public_url
    REDIS_URL                = "rediss://${aws_elasticache_replication_group.this.primary_endpoint_address}:6379"
    CLIENT_IP_HEADER         = "x-forwarded-for:last"
    CREDENTIALS_KEY_PROVIDER = "aws"
    CREDENTIALS_KMS_KEY      = aws_kms_key.this.arn
    AWS_REGION               = var.region
  }, var.extra_env) : { name = k, value = v }]
  logs = {
    logDriver = "awslogs"
    options = {
      awslogs-group         = aws_cloudwatch_log_group.api.name
      awslogs-region        = var.region
      awslogs-stream-prefix = "api"
    }
  }
}

resource "aws_ecs_task_definition" "api" {
  family                   = var.name
  requires_compatibilities = ["FARGATE"]
  network_mode             = "awsvpc"
  cpu                      = var.task_cpu
  memory                   = var.task_memory
  execution_role_arn       = aws_iam_role.execution.arn
  task_role_arn            = aws_iam_role.task.arn
  runtime_platform {
    operating_system_family = "LINUX"
    cpu_architecture        = "X86_64"
  }
  container_definitions = jsonencode([
    {
      name             = "migrate"
      image            = var.image
      essential        = false
      command          = ["agentraas-migrate"]
      secrets          = [for s in local.secrets : s if s.name == "DATABASE_URL"]
      logConfiguration = local.logs
    },
    {
      name             = "api"
      image            = var.image
      essential        = true
      portMappings     = [{ containerPort = 3000, protocol = "tcp" }]
      dependsOn        = [{ containerName = "migrate", condition = "SUCCESS" }]
      environment      = local.environment
      secrets          = local.secrets
      logConfiguration = local.logs
    },
  ])
}

resource "aws_ecs_service" "api" {
  name                              = var.name
  cluster                           = aws_ecs_cluster.this.id
  task_definition                   = aws_ecs_task_definition.api.arn
  desired_count                     = var.desired_count
  launch_type                       = "FARGATE"
  health_check_grace_period_seconds = 120
  network_configuration {
    subnets          = aws_subnet.public[*].id
    security_groups  = [aws_security_group.app.id]
    assign_public_ip = true
  }
  load_balancer {
    target_group_arn = aws_lb_target_group.api.arn
    container_name   = "api"
    container_port   = 3000
  }
  depends_on = [aws_lb_listener.http, aws_secretsmanager_secret_version.app]
}
