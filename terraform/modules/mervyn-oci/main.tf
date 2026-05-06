data "oci_identity_availability_domains" "ads" {
  compartment_id = var.tenancy_ocid
}

data "oci_core_images" "ubuntu_arm" {
  count = var.instance_source_image_id == "" && var.instance_image_os == "ubuntu" ? 1 : 0

  compartment_id           = var.compartment_ocid
  operating_system         = "Canonical Ubuntu"
  operating_system_version = var.ubuntu_version
  shape                    = var.instance_shape
  sort_by                  = "TIMECREATED"
  sort_order               = "DESC"
}

data "oci_core_images" "oracle_linux" {
  count = var.instance_source_image_id == "" && var.instance_image_os == "oracle-linux" ? 1 : 0

  compartment_id           = var.compartment_ocid
  operating_system         = "Oracle Linux"
  operating_system_version = var.oracle_linux_version
  shape                    = var.instance_shape
  sort_by                  = "TIMECREATED"
  sort_order               = "DESC"
}

locals {
  ad_name = data.oci_identity_availability_domains.ads.availability_domains[var.availability_domain_index].name

  instance_display_name = var.instance_display_name != "" ? var.instance_display_name : (
    var.instance_image_os == "oracle-linux" ? "${var.project_name}-vm" : "${var.project_name}-arm"
  )

  ssh_user_effective = var.ssh_user != null ? var.ssh_user : (
    var.instance_image_os == "oracle-linux" ? "opc" : "ubuntu"
  )

  # Prefer a non-Minimal image when several builds exist (better default for cloud-init / containers).
  ubuntu_non_minimal = length(data.oci_core_images.ubuntu_arm) > 0 ? [
    for img in data.oci_core_images.ubuntu_arm[0].images : img
    if !can(regex("Minimal", img.display_name))
  ] : []

  oracle_non_minimal = length(data.oci_core_images.oracle_linux) > 0 ? [
    for img in data.oci_core_images.oracle_linux[0].images : img
    if !can(regex("Minimal", img.display_name))
  ] : []

  image_id = var.instance_source_image_id != "" ? var.instance_source_image_id : (
    var.instance_image_os == "oracle-linux" ? (
      length(local.oracle_non_minimal) > 0 ? local.oracle_non_minimal[0].id : data.oci_core_images.oracle_linux[0].images[0].id
      ) : (
      length(local.ubuntu_non_minimal) > 0 ? local.ubuntu_non_minimal[0].id : data.oci_core_images.ubuntu_arm[0].images[0].id
    )
  )

  instance_metadata = merge(
    {
      ssh_authorized_keys = var.ssh_public_key
    },
    var.bootstrap_docker ? {
      user_data = base64encode(file(var.cloud_init_file))
    } : {}
  )
}

resource "oci_core_vcn" "this" {
  compartment_id = var.compartment_ocid
  cidr_blocks    = ["10.0.0.0/16"]
  display_name   = "${var.project_name}-vcn"
  dns_label      = replace(var.project_name, "-", "")
}

resource "oci_core_internet_gateway" "this" {
  compartment_id = var.compartment_ocid
  vcn_id         = oci_core_vcn.this.id
  display_name   = "${var.project_name}-igw"
  enabled        = true
}

resource "oci_core_default_route_table" "public" {
  manage_default_resource_id = oci_core_vcn.this.default_route_table_id
  display_name               = "${var.project_name}-public-rt"

  route_rules {
    destination       = "0.0.0.0/0"
    destination_type  = "CIDR_BLOCK"
    network_entity_id = oci_core_internet_gateway.this.id
  }
}

resource "oci_core_security_list" "public" {
  compartment_id = var.compartment_ocid
  vcn_id         = oci_core_vcn.this.id
  display_name   = "${var.project_name}-public-sl"

  dynamic "ingress_security_rules" {
    for_each = var.ssh_allowed_cidrs
    content {
      protocol = "6"
      source   = ingress_security_rules.value
      tcp_options {
        min = 22
        max = 22
      }
    }
  }

  dynamic "ingress_security_rules" {
    for_each = flatten([
      for cidr in var.http_cidrs : [
        { cidr = cidr, port = 80 },
        { cidr = cidr, port = 443 },
      ]
    ])
    content {
      protocol = "6"
      source   = ingress_security_rules.value.cidr
      tcp_options {
        min = ingress_security_rules.value.port
        max = ingress_security_rules.value.port
      }
    }
  }

  dynamic "ingress_security_rules" {
    for_each = var.expose_app_port ? var.app_port_cidrs : []
    content {
      protocol = "6"
      source   = ingress_security_rules.value
      tcp_options {
        min = 3000
        max = 3000
      }
    }
  }

  egress_security_rules {
    protocol    = "all"
    destination = "0.0.0.0/0"
  }
}

resource "oci_core_subnet" "public" {
  compartment_id             = var.compartment_ocid
  vcn_id                     = oci_core_vcn.this.id
  cidr_block                 = "10.0.0.0/24"
  display_name               = "${var.project_name}-public"
  dns_label                  = "${replace(var.project_name, "-", "")}pub"
  prohibit_public_ip_on_vnic = false
  route_table_id             = oci_core_vcn.this.default_route_table_id
  security_list_ids          = [oci_core_security_list.public.id]
}

resource "oci_core_instance" "mervyn" {
  compartment_id      = var.compartment_ocid
  availability_domain = local.ad_name
  display_name        = local.instance_display_name
  shape               = var.instance_shape

  shape_config {
    ocpus         = var.instance_ocpus
    memory_in_gbs = var.instance_memory_gbs
  }

  create_vnic_details {
    subnet_id        = oci_core_subnet.public.id
    assign_public_ip = true
  }

  source_details {
    source_type = "image"
    source_id   = local.image_id
  }

  metadata = local.instance_metadata

  preserve_boot_volume = false
}
