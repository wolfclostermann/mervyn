# AWS stack root. Composable layout aligns with:
# https://github.com/gfs-sre/terraform-framework-example-stack

module "aws_stack" {
  source = "../../modules/mervyn-aws"

  project_name             = var.project_name
  vpc_cidr_block           = var.vpc_cidr_block
  public_subnet_cidr_block = var.public_subnet_cidr_block
  instance_type            = var.instance_type
  ssh_public_key           = var.ssh_public_key
  ssh_allowed_cidrs        = var.ssh_allowed_cidrs
  http_cidrs               = var.http_cidrs
  expose_app_port          = var.expose_app_port
  app_port_cidrs           = var.app_port_cidrs
  bootstrap_docker         = var.bootstrap_docker
  ubuntu_version           = var.ubuntu_version
  cloud_init_file          = abspath("${path.module}/../../cloud-init-docker.yaml")
}
